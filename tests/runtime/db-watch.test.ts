// tests/runtime/db-watch.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import { diffRows } from '../../bin/lib/runtime/db-watch.js';

const rowsOf = (workspaces: Record<string, any>, panes: Record<string, any>) => ({
  version: 0,
  workspaces: new Map(Object.entries(workspaces)),
  panes: new Map(Object.entries(panes)),
});
const pane = (over: Record<string, any> = {}) => ({
  workspaceId: 'w1', agentType: 'shell', cwd: '/x', title: 'Terminal', color: '#6366f1', ...over,
});

describe('diffRows', () => {
  it('is empty when nothing changed', () => {
    const rows = rowsOf({ w1: { name: 'a', color: null } }, { p1: pane() });
    expect(diffRows(rows, rows)).toEqual([]);
  });

  it('announces a new space, then its color, which workspace.created cannot carry', () => {
    expect(diffRows(rowsOf({}, {}), rowsOf({ w2: { name: 'api', color: '#f00' } }, {}))).toEqual([
      ['workspace.created', { workspaceId: 'w2', name: 'api' }],
      ['workspace.updated', { workspaceId: 'w2', color: '#f00' }],
    ]);
  });

  it('reports only the fields of a space that changed', () => {
    const before = rowsOf({ w1: { name: 'a', color: '#111' } }, {});
    const after = rowsOf({ w1: { name: 'b', color: '#111' } }, {});
    expect(diffRows(before, after)).toEqual([['workspace.updated', { workspaceId: 'w1', name: 'b' }]]);
  });

  it('announces a new pane as not live, with its title and color', () => {
    const before = rowsOf({ w1: { name: 'a', color: null } }, {});
    const after = rowsOf({ w1: { name: 'a', color: null } }, { p1: pane() });
    expect(diffRows(before, after)).toEqual([
      ['pane.created', { paneId: 'p1', workspaceId: 'w1', agentType: 'shell', cwd: '/x', live: false }],
      ['pane.updated', { paneId: 'p1', title: 'Terminal', color: '#6366f1' }],
    ]);
  });

  it('reports a pane that changed space as moved', () => {
    const ws = { w1: { name: 'a', color: null }, w2: { name: 'b', color: null } };
    expect(diffRows(rowsOf(ws, { p1: pane() }), rowsOf(ws, { p1: pane({ workspaceId: 'w2' }) })))
      .toEqual([['pane.moved', { paneId: 'p1', workspaceId: 'w2' }]]);
  });

  it('reports a renamed pane', () => {
    const ws = { w1: { name: 'a', color: null } };
    expect(diffRows(rowsOf(ws, { p1: pane() }), rowsOf(ws, { p1: pane({ title: 'build' }) })))
      .toEqual([['pane.updated', { paneId: 'p1', title: 'build' }]]);
  });

  it('folds the panes of a deleted space into its workspace.removed', () => {
    const before = rowsOf({ w1: { name: 'a', color: null } }, { p1: pane(), p2: pane() });
    expect(diffRows(before, rowsOf({}, {}))).toEqual([
      ['workspace.removed', { workspaceId: 'w1', closedPanes: ['p1', 'p2'] }],
    ]);
  });

  it('reports a pane deleted on its own as removed', () => {
    const ws = { w1: { name: 'a', color: null } };
    expect(diffRows(rowsOf(ws, { p1: pane() }), rowsOf(ws, {})))
      .toEqual([['pane.removed', { paneId: 'p1' }]]);
  });
});

describe('the runtime replays outside writes to spaces.db', () => {
  let dir: string, dbPath: string, runtime: any, events: any[];

  beforeEach(() => {
    process.env.SPACES_NO_DETECTION_UPDATE = '1';
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-dbwatch-'));
    dbPath = path.join(dir, 'spaces.db');
    const db = new Database(dbPath);
    db.pragma('journal_mode = WAL');
    db.exec(`
      CREATE TABLE workspaces (
        id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT
      );
      CREATE TABLE panes (
        id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT 'Terminal', color TEXT NOT NULL DEFAULT '#6366f1',
        cwd TEXT NOT NULL DEFAULT '', agent_type TEXT DEFAULT 'shell', sort_order INTEGER DEFAULT 0,
        workspace_id INTEGER
      );
      INSERT INTO workspaces (name, color) VALUES ('home', NULL);
    `);
    db.close();
    runtime = createRuntime({ nodeId: 'test', spacesDbPath: dbPath, watchDb: true });
    // Driven by hand through tick(); each test is synchronous, so the
    // watcher's own timer never gets a turn in between.
    events = [];
    runtime.hub.subscribe((e: any) => events.push([e.type, e.payload]));
  });

  afterEach(() => {
    runtime.dbWatcher.stop();
    try { runtime.sessionStore.close(); } catch { /* already closed */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  // A second connection, standing in for the web app's process.
  const outside = (sql: string) => {
    const db = new Database(dbPath);
    try { db.exec(sql); } finally { db.close(); }
  };

  it('emits nothing when nobody wrote', () => {
    runtime.dbWatcher.tick();
    expect(events).toEqual([]);
  });

  it('turns a space renamed elsewhere into workspace.updated', () => {
    outside("UPDATE workspaces SET name = 'renamed' WHERE id = 1");
    runtime.dbWatcher.tick();
    expect(events).toEqual([['workspace.updated', { workspaceId: 'w1', name: 'renamed' }]]);
  });

  it('does not re-announce the runtime\'s own writes', () => {
    runtime.dispatch('workspace.create', { name: 'api' });
    runtime.dispatch('workspace.update', { workspaceId: 'w1', name: 'mine' });
    events.length = 0;
    runtime.dbWatcher.tick();
    outside("UPDATE workspaces SET color = '#0f0' WHERE id = 1");
    runtime.dbWatcher.tick();
    expect(events).toEqual([['workspace.updated', { workspaceId: 'w1', color: '#0f0' }]]);
  });

  it('shows a pane the web created in the next snapshot, not live', () => {
    outside("INSERT INTO panes (id, title, cwd, workspace_id) VALUES ('p9', 'Terminal', '/tmp', 1)");
    runtime.dbWatcher.tick();
    expect(events.map(([t]) => t)).toEqual(['pane.created', 'pane.updated']);
    expect(events[0][1]).toMatchObject({ paneId: 'p9', workspaceId: 'w1', live: false });
    const snap = runtime.dispatch('session.snapshot', {}).result;
    expect(snap.panes.find((p: any) => p.id === 'p9')).toMatchObject({ live: false, workspaceId: 'w1' });
  });

  it('applies a web rename to a running pane as a user title the shell cannot overwrite', () => {
    runtime.state.open('p1', { workspaceId: 'w1', agentType: 'shell', cwd: '/tmp' });
    outside("INSERT INTO panes (id, title, cwd, workspace_id) VALUES ('p1', 'Terminal', '/tmp', 1)");
    runtime.dbWatcher.tick();
    events.length = 0;
    outside("UPDATE panes SET title = 'build' WHERE id = 'p1'");
    runtime.dbWatcher.tick();
    expect(events).toEqual([['pane.updated', { paneId: 'p1', title: 'build' }]]);
    runtime.state.setTitle('p1', 'bash: ~/src');
    expect(runtime.state.get('p1').title).toBe('build');
  });

  it('leaves a running pane in place when only its row is deleted', () => {
    runtime.state.open('p1', { workspaceId: 'w1', agentType: 'shell', cwd: '/tmp' });
    outside("INSERT INTO panes (id, title, cwd, workspace_id) VALUES ('p1', 'Terminal', '/tmp', 1)");
    runtime.dbWatcher.tick();
    events.length = 0;
    outside("DELETE FROM panes WHERE id = 'p1'");
    runtime.dbWatcher.tick();
    expect(events).toEqual([]);
  });

  it('reports a space deleted elsewhere together with its panes', () => {
    outside("INSERT INTO panes (id, cwd, workspace_id) VALUES ('p2', '/tmp', 1)");
    runtime.dbWatcher.tick();
    events.length = 0;
    outside('DELETE FROM panes WHERE workspace_id = 1; DELETE FROM workspaces WHERE id = 1;');
    runtime.dbWatcher.tick();
    expect(events).toEqual([['workspace.removed', { workspaceId: 'w1', closedPanes: ['p2'] }]]);
  });
});
