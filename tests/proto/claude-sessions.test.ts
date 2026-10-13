// tests/proto/claude-sessions.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import {
  matchSessions, readClaudeSessions, createClaudeSessionWatcher, readProcessTree, readConsoleTree, readConsoleProcesses,
} from '../../bin/lib/runtime/claude-sessions.js';
import { createRuntime } from '../../bin/lib/runtime/index.js';

// pid → ppid. Pane A's shell is 100, pane B's is 200. 400 is a Claude
// started outside Spaces in the same folder, which must match no pane.
const tree = new Map([
  [100, 1], [101, 100],             // pane A: shell → claude
  [200, 1], [201, 200], [202, 201], // pane B: shell → wrapper → claude
  [400, 300], [300, 1],             // outside Spaces
]);
const panes = [{ paneId: 'A', pid: 100 }, { paneId: 'B', pid: 200 }];

describe('matchSessions', () => {
  it('names each pane\'s conversation by process ancestry, not by folder', () => {
    const found = matchSessions(panes, [
      { pid: 101, sessionId: 'sess-a' },
      { pid: 202, sessionId: 'sess-b' },
      { pid: 400, sessionId: 'sess-outside' },
    ], tree);
    expect(Object.fromEntries(found)).toEqual({ A: 'sess-a', B: 'sess-b' });
  });

  it('ignores a session file whose process has exited', () => {
    expect(matchSessions(panes, [{ pid: 999, sessionId: 'stale' }], tree).size).toBe(0);
  });

  it('terminates on a process table that loops', () => {
    const loop = new Map([[5, 6], [6, 5]]);
    expect(matchSessions(panes, [{ pid: 5, sessionId: 'x' }], loop).size).toBe(0);
  });
});

describe('readProcessTree', () => {
  it('gives null when ps fails', () => {
    let cmd = '';
    let got: any = 'unset';
    readProcessTree([], (t: any) => { got = t; }, ((c: string, _a: any, _o: any, cb: any) => {
      cmd = c; cb(new Error('no ps'), '');
    }) as any);
    expect(cmd).toBe('ps');
    expect(got).toBeNull();
  });

  it.skipIf(process.platform === 'win32')('reads this machine\'s real table', async () => {
    const t: Map<number, number> | null = await new Promise(r => readProcessTree([], r));
    expect(t?.get(process.pid)).toBe(process.ppid);
  });
});

describe('readConsoleTree (Windows)', () => {
  // Pane A's console holds the shell, a shim and the agent; pane B's could
  // not be read. Everything in a console hangs off its pane's pid.
  const consoles: Record<number, number[]> = { 100: [100, 150, 101], 200: [] };
  const readConsole = (pid: number, cb: (l: number[]) => void) => cb(consoles[pid] || []);

  it('maps every process in a pane\'s console to that pane', () => {
    let tree: Map<number, number> | null = null;
    readConsoleTree(panes, (t: any) => { tree = t; }, readConsole);
    expect(Object.fromEntries(tree!)).toEqual({ 150: 100, 101: 100 });
    const found = matchSessions(panes, [{ pid: 101, sessionId: 'a' }, { pid: 400, sessionId: 'x' }], tree!);
    expect(Object.fromEntries(found)).toEqual({ A: 'a' });
  });

  it('is null when no console could be read, so the watcher tries again', () => {
    let tree: any = 'unset';
    readConsoleTree([{ paneId: 'B', pid: 200 }], (t: any) => { tree = t; }, readConsole);
    expect(tree).toBeNull();
  });

  it('gives up on a helper that never answers', async () => {
    const { EventEmitter } = await import('node:events');
    let killed = false;
    const forkFn = () => Object.assign(new EventEmitter(), { kill: () => { killed = true; } });
    const list: number[] = await new Promise(r => readConsoleProcesses(100, r, forkFn as any));
    expect(list).toEqual([]);
    expect(killed).toBe(true);
  }, 10000);

  it.skipIf(process.platform !== 'win32')('finds a process started through an exec shim in a real pane', async () => {
    // The case ancestry misses: Git Bash → sh shim → exec'd program.
    const pty = await import('node-pty');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-shim-'));
    fs.writeFileSync(path.join(dir, 'child.js'), "console.log('CHILD ' + process.pid); setInterval(() => {}, 1000)");
    fs.writeFileSync(path.join(dir, 'fake'), `#!/bin/sh\nexec node "${path.join(dir, 'child.js').replace(/\\/g, '/')}"\n`);
    const bash = ['C:/Program Files/Git/bin/bash.exe', 'C:/Program Files/Git/usr/bin/bash.exe'].find(b => fs.existsSync(b));
    if (!bash) return;
    const term = pty.spawn(bash, [], { name: 'xterm-256color', cols: 80, rows: 24, cwd: os.tmpdir(), env: { ...process.env, PATH: `${dir};${process.env.PATH}` } });
    let out = '';
    term.onData((d: string) => { out += d; });
    try {
      term.write('fake\r');
      const deadline = Date.now() + 10000;
      while (!/CHILD (\d+)/.test(out) && Date.now() < deadline) await new Promise(r => setTimeout(r, 200));
      const child = Number(/CHILD (\d+)/.exec(out)![1]);
      const tree: Map<number, number> = await new Promise(r => readConsoleTree([{ paneId: 'A', pid: term.pid }], r));
      const found = matchSessions([{ paneId: 'A', pid: term.pid }], [{ pid: child, sessionId: 's' }], tree);
      expect(Object.fromEntries(found)).toEqual({ A: 's' });
    } finally {
      term.kill();
      // The shell lets go of the folder a moment after the kill.
      await new Promise(r => setTimeout(r, 500));
      try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* next temp sweep */ }
    }
  }, 20000);
});

describe('readClaudeSessions', () => {
  let home: string;
  beforeEach(() => {
    home = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-cs-'));
    const dir = path.join(home, '.claude', 'sessions');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, '101.json'), JSON.stringify({ pid: 101, sessionId: 'abc-1', kind: 'interactive' }));
    fs.writeFileSync(path.join(dir, '102.json'), JSON.stringify({ pid: 102, sessionId: 'abc-2', kind: 'print' }));
    fs.writeFileSync(path.join(dir, '103.json'), '{ half-written');
    fs.writeFileSync(path.join(dir, '104.json'), JSON.stringify({ pid: 104, sessionId: '../../evil' }));
    fs.writeFileSync(path.join(dir, '105.key'), 'secret');
  });
  afterEach(() => fs.rmSync(home, { recursive: true, force: true }));

  it('reads interactive sessions and skips everything else', () => {
    expect(readClaudeSessions(home)).toEqual([{ pid: 101, sessionId: 'abc-1' }]);
  });

  it('is empty when Claude has never run', () => {
    expect(readClaudeSessions(path.join(home, 'nobody'))).toEqual([]);
  });
});

describe('createClaudeSessionWatcher', () => {
  it('reports a pane\'s session once, and again only when it changes', () => {
    const reported: string[] = [];
    let current: string | null = null;
    let sessions = [{ pid: 101, sessionId: 'first' }];
    const w = createClaudeSessionWatcher({
      listPanes: () => [{ paneId: 'A', pid: 100, sessionId: current }],
      onSession: (paneId: string, id: string) => { reported.push(`${paneId}:${id}`); current = id; },
      readTree: (_p: any, cb: any) => cb(tree),
      readSessions: () => sessions,
      intervalMs: 60_000,
    });
    try {
      w.tick(); w.tick();
      sessions = [{ pid: 101, sessionId: 'second' }]; // /resume inside Claude
      w.tick();
      expect(reported).toEqual(['A:first', 'A:second']);
    } finally {
      w.stop();
    }
  });

  it('reads the process table once per session process, not every tick', () => {
    // On Windows each read starts PowerShell; /resume keeps the pid.
    let reads = 0;
    let current: string | null = null;
    let sessions = [{ pid: 101, sessionId: 'first' }, { pid: 400, sessionId: 'outside' }];
    const w = createClaudeSessionWatcher({
      listPanes: () => [{ paneId: 'A', pid: 100, sessionId: current }],
      onSession: (_: string, id: string) => { current = id; },
      readTree: (_p: any, cb: any) => { reads++; cb(tree); },
      readSessions: () => sessions,
      intervalMs: 60_000,
    });
    try {
      w.tick(); w.tick(); w.tick();
      expect(reads).toBe(1);
      sessions = [{ pid: 101, sessionId: 'second' }, { pid: 400, sessionId: 'outside' }];
      w.tick();
      expect(current).toBe('second');
      expect(reads).toBe(1);
      sessions = [...sessions, { pid: 202, sessionId: 'new-process' }];
      w.tick();
      expect(reads).toBe(2);
    } finally {
      w.stop();
    }
  });

  it('looks again when a pane is added', () => {
    let reads = 0;
    let list = [{ paneId: 'A', pid: 100, sessionId: null as string | null }];
    const reported: string[] = [];
    const w = createClaudeSessionWatcher({
      listPanes: () => list,
      onSession: (p: string, id: string) => reported.push(`${p}:${id}`),
      readTree: (_p: any, cb: any) => { reads++; cb(tree); },
      readSessions: () => [{ pid: 202, sessionId: 'b' }],
      intervalMs: 60_000,
    });
    try {
      w.tick();
      list = [...list, { paneId: 'B', pid: 200, sessionId: null }];
      w.tick();
      expect(reads).toBe(2);
      expect(reported).toEqual(['B:b']);
    } finally {
      w.stop();
    }
  });

  it('does nothing where the process table cannot be read', () => {
    const reported: string[] = [];
    const w = createClaudeSessionWatcher({
      listPanes: () => [{ paneId: 'A', pid: 100, sessionId: null }],
      onSession: (p: string) => reported.push(p),
      readTree: (_p: any, cb: any) => cb(null),
      readSessions: () => [{ pid: 101, sessionId: 'x' }],
      intervalMs: 60_000,
    });
    w.tick(); w.stop();
    expect(reported).toEqual([]);
  });
});

describe('pane.restart resumes a recorded conversation', () => {
  let dir: string, home: string, saved: Record<string, string | undefined>;
  beforeEach(() => {
    saved = { HOME: process.env.HOME, USERPROFILE: process.env.USERPROFILE };
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-resume-'));
    home = path.join(dir, 'home');
    fs.mkdirSync(path.join(home, '.claude', 'projects', '-work'), { recursive: true });
    fs.writeFileSync(path.join(home, '.claude', 'projects', '-work', 'abc-123.jsonl'), '{}');
    process.env.HOME = home; process.env.USERPROFILE = home;
    process.env.SPACES_NO_DETECTION_UPDATE = '1';
    const db = new Database(path.join(dir, 'spaces.db'));
    db.exec(`
      CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT);
      CREATE TABLE panes (id TEXT PRIMARY KEY, title TEXT, color TEXT, cwd TEXT, agent_type TEXT,
        sort_order INTEGER DEFAULT 0, claude_session_id TEXT, custom_command TEXT, workspace_id INTEGER);
      INSERT INTO workspaces (name) VALUES ('dev');
    `);
    db.prepare("INSERT INTO panes (id, cwd, agent_type, workspace_id) VALUES ('p', ?, 'claude', 1)").run(process.cwd());
    db.close();
  });
  afterEach(() => {
    for (const [k, v] of Object.entries(saved)) { if (v === undefined) delete process.env[k]; else process.env[k] = v; }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('records the session through the store, then resumes it on restart', async () => {
    const rt = createRuntime({ nodeId: 'test', spacesDbPath: path.join(dir, 'spaces.db') });
    try {
      expect(rt.sessionStore.setClaudeSession('p', 'abc-123').ok).toBe(true);
      const res = rt.dispatch('pane.restart', { paneId: 'p' }) as any;
      expect(res.result).toMatchObject({ resumed: true, launchCommand: 'claude --resume abc-123' });
      rt.dispatch('pane.close', { paneId: 'p' });
      await new Promise(r => setTimeout(r, 300));
    } finally {
      try { rt.sessionStore.close(); } catch { /* */ }
    }
  }, 15000);

  it('starts fresh when the recorded transcript is gone', async () => {
    const rt = createRuntime({ nodeId: 'test', spacesDbPath: path.join(dir, 'spaces.db') });
    try {
      rt.sessionStore.setClaudeSession('p', 'gone-999');
      const res = rt.dispatch('pane.restart', { paneId: 'p' }) as any;
      expect(res.result).toMatchObject({ resumed: false, launchCommand: 'claude' });
      rt.dispatch('pane.close', { paneId: 'p' });
      await new Promise(r => setTimeout(r, 300));
    } finally {
      try { rt.sessionStore.close(); } catch { /* */ }
    }
  }, 15000);
});
