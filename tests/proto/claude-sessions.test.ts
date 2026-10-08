// tests/proto/claude-sessions.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import {
  matchSessions, readClaudeSessions, createClaudeSessionWatcher,
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
      readTree: (cb: any) => cb(tree),
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

  it('does nothing where the process table cannot be read', () => {
    const reported: string[] = [];
    const w = createClaudeSessionWatcher({
      listPanes: () => [{ paneId: 'A', pid: 100, sessionId: null }],
      onSession: (p: string) => reported.push(p),
      readTree: (cb: any) => cb(null),
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
