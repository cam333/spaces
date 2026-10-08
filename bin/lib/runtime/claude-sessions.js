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
// Recorded so that a pane stopped by a server restart or a reboot resumes
// its conversation (pane.restart) rather than starting a new one.

const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFile } = require('child_process');

const POLL_MS = 5000;
// Deeper than any shell → wrapper → node → claude chain, and a bound on a
// corrupt table that loops.
const MAX_DEPTH = 32;

/** pid → ppid from `ps`, or null where that is unavailable (Windows). */
function readProcessTree(callback, run = execFile) {
  if (process.platform === 'win32') return callback(null);
  run('ps', ['-A', '-o', 'pid=,ppid='], { timeout: 3000 }, (error, stdout) => {
    if (error) return callback(null);
    const tree = new Map();
    for (const line of String(stdout).split('\n')) {
      const [pid, ppid] = line.trim().split(/\s+/).map(Number);
      if (Number.isInteger(pid) && Number.isInteger(ppid)) tree.set(pid, ppid);
    }
    callback(tree);
  });
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
  readTree = readProcessTree, readSessions = readClaudeSessions,
}) {
  let busy = false;
  function tick() {
    if (busy) return;
    const panes = listPanes();
    if (!panes.length) return;
    busy = true;
    readTree((tree) => {
      busy = false;
      if (!tree) return;
      for (const [paneId, sessionId] of matchSessions(panes, readSessions(home), tree)) {
        const pane = panes.find(p => p.paneId === paneId);
        if (pane && pane.sessionId !== sessionId) {
          try { onSession(paneId, sessionId); } catch { /* bookkeeping only */ }
        }
      }
    });
  }
  const timer = setInterval(tick, intervalMs);
  timer.unref?.();
  return { tick, stop: () => clearInterval(timer) };
}

module.exports = { createClaudeSessionWatcher, matchSessions, readClaudeSessions, readProcessTree };
