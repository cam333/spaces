'use strict';

// Which Claude conversation each pane is running.
//
// Claude Code writes ~/.claude/sessions/<pid>.json for every interactive
// process and keeps its `sessionId` current through /resume and /clear. A
// pane runs Claude as a descendant of its shell, so walking each session
// file's pid up the process tree to a pane's shell names that pane's
// conversation exactly. Guessing from the newest transcript in the pane's
// folder instead picks up any other Claude working in the same folder.
//
// Windows has no `ps`, and ancestry breaks there anyway: the `claude` shim
// is a shell script that `exec`s the real program, and Git Bash leaves a
// dead pid in the chain when it does. What every process in a pane does
// share is the pane's console, so on Windows the pane is found by asking
// which processes are attached to its console instead.
//
// Recorded so that a pane stopped by a server restart or a reboot resumes
// its conversation (pane.restart) rather than starting a new one.

const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFile, fork } = require('child_process');

const POLL_MS = 5000;
// Deeper than any shell → wrapper → node → claude chain, and a bound on a
// corrupt table that loops.
const MAX_DEPTH = 32;
// A console-list helper that hangs is given up on; the pane stays unplaced
// and is looked at again next tick.
const CONSOLE_LIST_MS = 5000;

/**
 * pid → ppid from `ps`, or null when it cannot be read. `panes` is unused
 * here; the Windows reader takes the same arguments.
 */
function readProcessTree(panes, callback, run = execFile) {
  run('ps', ['-A', '-o', 'pid=,ppid='], { timeout: 3000 }, (error, stdout) => {
    if (error) return callback(null);
    const tree = new Map();
    for (const line of String(stdout).split('\n')) {
      const [pid, ppid] = line.trim().split(/\s+/).map(Number);
      if (Number.isInteger(pid) && Number.isInteger(ppid)) tree.set(pid, ppid);
    }
    callback(tree.size ? tree : null);
  });
}

// node-pty ships this helper for its own kill(): it must run in a separate
// process, since a process can attach to only one console at a time.
function consoleListAgent() {
  try { return require.resolve('node-pty/lib/conpty_console_list_agent'); } catch { return null; }
}

/** The pids attached to the console of `pid`, or [] when that cannot be read. */
function readConsoleProcesses(pid, callback, forkProcess = fork) {
  const agentPath = consoleListAgent();
  if (!agentPath) return callback([]);
  let agent;
  try {
    agent = forkProcess(agentPath, [String(pid)], { stdio: ['ignore', 'ignore', 'ignore', 'ipc'], windowsHide: true });
  } catch {
    return callback([]);
  }
  let done = false;
  const finish = (list) => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    callback(list);
  };
  const timer = setTimeout(() => { try { agent.kill(); } catch { /* gone */ } finish([]); }, CONSOLE_LIST_MS);
  agent.on('message', (m) => finish(Array.isArray(m?.consoleProcessList) ? m.consoleProcessList.filter(Number.isInteger) : []));
  agent.on('error', () => finish([]));
  agent.on('exit', () => finish([]));
}

/**
 * The Windows reader: a tree in which every process attached to a pane's
 * console hangs directly off that pane's pid, so matchSessions needs one
 * step. Null when no console could be read, so the watcher tries again.
 */
function readConsoleTree(panes, callback, readConsole = readConsoleProcesses) {
  const shells = panes.filter(p => Number.isInteger(p.pid));
  if (!shells.length) return callback(null);
  const tree = new Map();
  let pending = shells.length;
  let anyRead = false;
  for (const { pid } of shells) {
    readConsole(pid, (list) => {
      if (list.length) anyRead = true;
      for (const member of list) if (member !== pid) tree.set(member, pid);
      if (--pending === 0) callback(anyRead ? tree : null);
    });
  }
}

/** `{ pid, sessionId }` for every interactive Claude session file. */
function readClaudeSessions(home, fsMod = fs) {
  const dir = path.join(home, '.claude', 'sessions');
  let names;
  try { names = fsMod.readdirSync(dir); } catch { return []; }
  const out = [];
  for (const name of names) {
    if (!/^\d+\.json$/.test(name)) continue;
    try {
      const data = JSON.parse(fsMod.readFileSync(path.join(dir, name), 'utf-8'));
      if (data.kind && data.kind !== 'interactive') continue;
      if (Number.isInteger(data.pid) && typeof data.sessionId === 'string' && /^[0-9a-zA-Z-]+$/.test(data.sessionId)) {
        out.push({ pid: data.pid, sessionId: data.sessionId });
      }
    } catch { /* being rewritten, or not Claude's */ }
  }
  return out;
}

/**
 * paneId → sessionId for every session whose process descends from a pane's
 * shell. `panes` is `[{ paneId, pid }]`. A session file whose process has
 * exited is not in `tree` and so matches nothing.
 */
function matchSessions(panes, sessions, tree) {
  const paneOfPid = new Map(panes.filter(p => Number.isInteger(p.pid)).map(p => [p.pid, p.paneId]));
  const found = new Map();
  for (const { pid, sessionId } of sessions) {
    let at = pid;
    for (let depth = 0; depth < MAX_DEPTH && tree.has(at); depth++) {
      at = tree.get(at);
      if (paneOfPid.has(at)) {
        found.set(paneOfPid.get(at), sessionId);
        break;
      }
    }
  }
  return found;
}

/**
 * Polls while any pane is live and reports each change of a pane's session.
 *
 * @param {object} opts
 * @param {() => Array<{paneId: string, pid: number, sessionId: string|null}>} opts.listPanes
 *   the live panes, with the session last reported for each
 * @param {(paneId: string, sessionId: string) => void} opts.onSession
 */
function createClaudeSessionWatcher({
  listPanes, onSession, home = os.homedir(), intervalMs = POLL_MS,
  readTree = process.platform === 'win32' ? readConsoleTree : readProcessTree,
  readSessions = readClaudeSessions,
}) {
  let busy = false;
  // A process's parents are fixed when it starts, so a session's pid, once
  // placed in a pane (or in none), needs no second look at the process
  // table: /resume and /clear change the sessionId under the same pid.
  // Forgotten when the set of panes changes, since a pane listed late may be
  // the one a session already looked at belongs to.
  const placed = new Map(); // pid → paneId | null
  let placedFor = '';

  function report(panes, found) {
    for (const [paneId, sessionId] of found) {
      const pane = panes.find(p => p.paneId === paneId);
      if (pane && pane.sessionId !== sessionId) {
        try { onSession(paneId, sessionId); } catch { /* bookkeeping only */ }
      }
    }
  }

  function tick() {
    if (busy) return;
    const panes = listPanes();
    if (!panes.length) return;
    const key = panes.map(p => `${p.paneId}:${p.pid}`).sort().join(',');
    if (key !== placedFor) { placed.clear(); placedFor = key; }
    const sessions = readSessions(home);
    const current = new Set(sessions.map(s => s.pid));
    for (const pid of placed.keys()) if (!current.has(pid)) placed.delete(pid);

    const known = new Map();
    for (const { pid, sessionId } of sessions) {
      const paneId = placed.get(pid);
      if (paneId) known.set(paneId, sessionId);
    }
    report(panes, known);
    const unplaced = sessions.filter(s => !placed.has(s.pid));
    if (!unplaced.length) return;

    busy = true;
    readTree(panes, (tree) => {
      busy = false;
      if (!tree) return;
      for (const s of unplaced) {
        const [paneId] = matchSessions(panes, [s], tree).keys();
        placed.set(s.pid, paneId ?? null);
      }
      report(panes, matchSessions(panes, unplaced, tree));
    });
  }
  const timer = setInterval(tick, intervalMs);
  timer.unref?.();
  return { tick, stop: () => clearInterval(timer) };
}

module.exports = {
  createClaudeSessionWatcher, matchSessions, readClaudeSessions,
  readProcessTree, readConsoleTree, readConsoleProcesses,
};
