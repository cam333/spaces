// tests/proto/workspace-control.test.ts
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';

let dir: string, dbPath: string;

beforeEach(() => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-wc-'));
  dbPath = path.join(dir, 'spaces.db');
  const db = new Database(dbPath);
  db.exec(`
    CREATE TABLE workspaces (
      id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT
    );
    CREATE TABLE panes (
      id TEXT PRIMARY KEY, title TEXT, color TEXT, cwd TEXT,
      agent_type TEXT DEFAULT 'shell', sort_order INTEGER DEFAULT 0,
      workspace_id INTEGER REFERENCES workspaces(id) ON DELETE CASCADE
    );
  `);
  db.close();
});

// Windows cannot remove a directory containing a file with an open handle.
// createSessionStore() keeps its better-sqlite3 connection open for the
// runtime's lifetime by design (see session-store.js) and close() is
// terminal, so it can't be closed mid-dispatch — track every runtime
// created in a test and close its store here, before rmSync, the same way
// session-control.test.ts and session-store.test.ts do.
let runtimes: any[] = [];

afterEach(() => {
  for (const runtime of runtimes.splice(0)) {
    try { runtime.sessionStore.close(); } catch { /* already closed */ }
  }
  fs.rmSync(dir, { recursive: true, force: true });
});

const rt = () => {
  const runtime = createRuntime({ nodeId: 'test', spacesDbPath: dbPath });
  runtimes.push(runtime);
  return runtime;
};
const rows = (sql: string) => {
  const db = new Database(dbPath, { readonly: true });
  try { return db.prepare(sql).all(); } finally { db.close(); }
};
const settle = (ms = 400) => new Promise(r => setTimeout(r, ms));

describe('workspace.create', () => {
  it('creates a workspace and returns its public id', () => {
    const runtime = rt();
    const res = runtime.dispatch('workspace.create', { name: 'frontend', color: '#89b4fa' }) as any;
    expect(res.result.workspaceId).toMatch(/^w\d+$/);
    expect(rows('SELECT name, color FROM workspaces')).toEqual([{ name: 'frontend', color: '#89b4fa' }]);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.workspaces.find((w: any) => w.id === res.result.workspaceId)).toMatchObject({ name: 'frontend' });
  });

  it('emits workspace.created', () => {
    const runtime = rt();
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));
    const res = runtime.dispatch('workspace.create', { name: 'api' }) as any;
    expect(events.some(e => e.type === 'workspace.created' && e.payload.workspaceId === res.result.workspaceId)).toBe(true);
  });

  it('rejects a blank name', () => {
    const runtime = rt();
    expect((runtime.dispatch('workspace.create', {}) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('workspace.create', { name: '  ' }) as any).error.code).toBe('bad_request');
  });
});

describe('workspace.update', () => {
  it('renames a workspace and emits workspace.updated', () => {
    const runtime = rt();
    const { workspaceId } = (runtime.dispatch('workspace.create', { name: 'old' }) as any).result;
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));
    expect((runtime.dispatch('workspace.update', { workspaceId, name: 'new' }) as any).result.ok).toBe(true);
    expect(rows('SELECT name FROM workspaces')).toEqual([{ name: 'new' }]);
    expect(events.some(e => e.type === 'workspace.updated')).toBe(true);
  });

  it('rejects an update with no fields and an unknown workspace', () => {
    const runtime = rt();
    const { workspaceId } = (runtime.dispatch('workspace.create', { name: 'x' }) as any).result;
    expect((runtime.dispatch('workspace.update', { workspaceId }) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('workspace.update', { workspaceId: 'w9999', name: 'y' }) as any).error.code).toBe('not_found');
    expect((runtime.dispatch('workspace.update', { workspaceId: 'nope', name: 'y' }) as any).error.code).toBe('bad_request');
  });
});

describe('workspace.close', () => {
  it('closes live panes then removes the workspace', async () => {
    const runtime = rt();
    const { workspaceId } = (runtime.dispatch('workspace.create', { name: 'doomed' }) as any).result;
    const a = (runtime.dispatch('pane.create', { workspaceId, agentType: 'shell', cwd: process.cwd() }) as any).result;
    const b = (runtime.dispatch('pane.create', { workspaceId, agentType: 'shell', cwd: process.cwd() }) as any).result;
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));

    const res = runtime.dispatch('workspace.close', { workspaceId }) as any;
    expect(res.result.ok).toBe(true);
    expect(res.result.closedPanes.sort()).toEqual([a.paneId, b.paneId].sort());
    expect(rows('SELECT id FROM workspaces')).toEqual([]);
    expect(rows('SELECT id FROM panes')).toEqual([]);
    expect(events.some(e => e.type === 'workspace.removed')).toBe(true);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.filter((p: any) => p.live)).toEqual([]);
    await settle();
  }, 20000);

  // Regression test: previously a closed workspace's state records survived
  // (live:false) and snapshot()'s backfill loop recreated a nameless stub
  // workspace around them, resurrecting a workspace the event stream had just
  // reported removed. Both the workspace AND its panes must be entirely absent
  // from the next snapshot, not just live:false.
  it('removes the workspace and its panes entirely from the next snapshot — no ghost stub', async () => {
    const runtime = rt();
    const { workspaceId } = (runtime.dispatch('workspace.create', { name: 'doomed2' }) as any).result;
    const pane = (runtime.dispatch('pane.create', { workspaceId, agentType: 'shell', cwd: process.cwd() }) as any).result;

    const res = runtime.dispatch('workspace.close', { workspaceId }) as any;
    expect(res.result.ok).toBe(true);

    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.workspaces.find((w: any) => w.id === workspaceId)).toBeUndefined();
    expect(snap.panes.find((p: any) => p.id === pane.paneId)).toBeUndefined();
    await settle();
  }, 20000);

  it('reports not_found for an unknown workspace', () => {
    const runtime = rt();
    expect((runtime.dispatch('workspace.close', { workspaceId: 'w4242' }) as any).error.code).toBe('not_found');
  });

  it('kills no live pane when the workspace row was deleted out-of-band', async () => {
    const runtime = rt();
    const { workspaceId } = (runtime.dispatch('workspace.create', { name: 'ghost' }) as any).result;
    const pane = (runtime.dispatch('pane.create', { workspaceId, agentType: 'shell', cwd: process.cwd() }) as any).result;

    // Model the web app deleting the workspace row directly — plausible,
    // since the runtime and the web app share spaces.db — while the
    // runtime's own live pane record and PTY know nothing about it. Using a
    // fresh connection (default foreign_keys=OFF) means this does NOT
    // cascade-delete the pane row, leaving it orphaned but intact, exactly
    // like production's non-cascading schema (see session-store.js).
    const raw = new Database(dbPath);
    raw.prepare('DELETE FROM workspaces WHERE id = ?').run(Number(workspaceId.slice(1)));
    raw.close();

    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));
    const res = runtime.dispatch('workspace.close', { workspaceId }) as any;

    expect(res.error?.code).toBe('not_found');
    // The pane must still be alive: existence is checked before anything is
    // killed, so a stale/foreign-deleted workspace id must never take a
    // live PTY down with it.
    expect(events.some(e => e.type === 'pane.closed')).toBe(false);
    expect(events.some(e => e.type === 'workspace.removed')).toBe(false);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.find((p: any) => p.id === pane.paneId)?.live).toBe(true);

    runtime.dispatch('pane.close', { paneId: pane.paneId }); // clean up the still-live PTY
    await settle();
  }, 20000);

  it('kills no panes and reports internal on a genuine closeWorkspace failure', async () => {
    const runtime = rt();
    const { workspaceId } = (runtime.dispatch('workspace.create', { name: 'flaky' }) as any).result;
    const pane = (runtime.dispatch('pane.create', { workspaceId, agentType: 'shell', cwd: process.cwd() }) as any).result;

    const spy = vi
      .spyOn(runtime.sessionStore, 'closeWorkspace')
      .mockReturnValueOnce({ ok: false, error: 'disk I/O error' });
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));

    const res = runtime.dispatch('workspace.close', { workspaceId }) as any;

    expect(res.error?.code).toBe('internal');
    // Same contract as the out-of-band case above: any closeWorkspace
    // failure — not just not_found — means nothing happened, distinguishing
    // it from a real close where closedPanes reports what was killed.
    expect(events.some(e => e.type === 'pane.closed')).toBe(false);
    expect(events.some(e => e.type === 'workspace.removed')).toBe(false);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.find((p: any) => p.id === pane.paneId)?.live).toBe(true);

    spy.mockRestore();
    runtime.dispatch('pane.close', { paneId: pane.paneId }); // clean up the still-live PTY
    await settle();
  }, 20000);
});

describe('pane.move', () => {
  it('moves a live pane and the snapshot follows', async () => {
    const runtime = rt();
    const from = (runtime.dispatch('workspace.create', { name: 'from' }) as any).result.workspaceId;
    const to = (runtime.dispatch('workspace.create', { name: 'to' }) as any).result.workspaceId;
    const pane = (runtime.dispatch('pane.create', { workspaceId: from, agentType: 'shell', cwd: process.cwd() }) as any).result;
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));

    expect((runtime.dispatch('pane.move', { paneId: pane.paneId, workspaceId: to }) as any).result.ok).toBe(true);
    expect(events.some(e => e.type === 'pane.moved' && e.payload.workspaceId === to)).toBe(true);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.find((p: any) => p.id === pane.paneId).workspaceId).toBe(to);
    expect(snap.workspaces.find((w: any) => w.id === to).paneIds).toContain(pane.paneId);
    runtime.dispatch('pane.close', { paneId: pane.paneId });
    await settle();
  }, 20000);

  it('rejects unknown pane, unknown workspace, and malformed ids', () => {
    const runtime = rt();
    const to = (runtime.dispatch('workspace.create', { name: 'to' }) as any).result.workspaceId;
    expect((runtime.dispatch('pane.move', { paneId: 'ghost', workspaceId: to }) as any).error.code).toBe('not_found');
    expect((runtime.dispatch('pane.move', { paneId: 'x', workspaceId: 'bogus' }) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('pane.move', {}) as any).error.code).toBe('bad_request');
  });
});
