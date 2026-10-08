import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import { schemas } from '../../bin/lib/runtime/protocol.js';

let dbPath: string;

beforeEach(() => {
  dbPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-rt-')), 'spaces.db');
  const db = new Database(dbPath);
  // Minimal mirror of the production schema (verify against src/lib/db/schema.ts)
  db.exec(`
    CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, color TEXT);
    CREATE TABLE panes (id TEXT PRIMARY KEY, workspace_id INTEGER, agent_type TEXT, cwd TEXT, title TEXT, color TEXT);
    INSERT INTO workspaces (id, name, color) VALUES (1, 'frontend', '#89b4fa');
    INSERT INTO panes (id, workspace_id, agent_type, cwd, title, color) VALUES ('p1', 1, 'claude', 'C:\\work', '', NULL);
  `);
  db.close();
});

afterEach(() => { fs.rmSync(path.dirname(dbPath), { recursive: true, force: true }); });

describe('createRuntime', () => {
  it('snapshot merges DB structure with live runtime state', () => {
    const rt = createRuntime({ nodeId: 'test-node', spacesDbPath: dbPath });
    rt.registerPane('p1', {
      cols: 80, rows: 24, agentType: 'claude', cwd: 'C:\\work', workspaceId: 'w1',
      onInput: () => {}, onResize: () => {},
    });
    rt.state.setAgentState('p1', 'working');
    const snap = rt.snapshot();
    expect(snap.protocolVersion).toBe(1);
    expect(snap.nodeId).toBe('test-node');
    expect(snap.workspaces).toEqual([{ id: 'w1', name: 'frontend', color: '#89b4fa', paneIds: ['p1'] }]);
    const p1 = snap.panes.find(p => p.id === 'p1')!;
    expect(p1.agentState).toBe('working');
    expect(p1.live).toBe(true);
  });

  it('snapshot lists DB panes with no live session as live=false, state unknown', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    const snap = rt.snapshot();
    const p1 = snap.panes.find(p => p.id === 'p1')!;
    expect(p1.live).toBe(false);
    expect(p1.agentState).toBe('unknown');
  });

  it('feed routes PTY output to screen and data subscribers', async () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
    const chunks: string[] = [];
    rt.subscribePaneData('p1', c => chunks.push(c));
    rt.feed('p1', 'hello\r\n');
    await new Promise(r => setTimeout(r, 50)); // xterm parse is async
    expect(chunks).toEqual(['hello\r\n']);
    expect(rt.getScreen('p1')!.visibleGrid()[0]).toContain('hello');
  });

  it('writePane and resizePane invoke the registered callbacks', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    const inputs: string[] = []; const sizes: number[][] = [];
    rt.registerPane('p1', {
      cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1',
      onInput: d => inputs.push(d), onResize: (c, r) => sizes.push([c, r]),
    });
    expect(rt.writePane('p1', 'ls\r')).toBe(true);
    expect(rt.resizePane('p1', 100, 30)).toBe(true);
    expect(rt.writePane('nope', 'x')).toBe(false);
    expect(inputs).toEqual(['ls\r']);
    expect(sizes).toEqual([[100, 30]]);
  });

  it('snapshot backfills a stub workspace for a live pane the DB never provided', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: '' });
    rt.registerPane('p1', {
      cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1',
      onInput: () => {}, onResize: () => {},
    });
    const snap = rt.snapshot();
    const ws = snap.workspaces.find(w => w.id === 'w1');
    expect(ws).toBeDefined();
    expect(ws!.paneIds).toContain('p1');
    expect(schemas.Snapshot.safeParse(snap).success).toBe(true);
  });

  // Safeguard: the fix for closed-workspace resurrection (state.remove() on
  // protocol-initiated close) must NOT change what a natural PTY exit shows for
  // a pane whose workspace the DB still knows about — that pane keeps showing
  // up as live:false, still listed under its (still-real, non-stub) workspace,
  // exactly as before.
  it('a natural PTY exit still shows the pane live:false under its still-known workspace', () => {
    const rtInstance = createRuntime({ nodeId: 'test-node', spacesDbPath: dbPath });
    // p1/workspace 1 are DB-seeded in beforeEach — a live pane registered
    // with a real, still-existing DB row, exactly the realistic case (both
    // spawn paths always create the DB row before/alongside registerPane).
    rtInstance.registerPane('p1', {
      cols: 80, rows: 24, agentType: 'claude', cwd: 'C:\\work', workspaceId: 'w1',
      onInput: () => {}, onResize: () => {},
    });
    // Simulates the PTY's own onExit callback (spawn-pane.js's
    // term.onExit -> runtime.paneExit), NOT a protocol-initiated close.
    rtInstance.paneExit('p1', 0);
    const snap = rtInstance.snapshot();
    const p1 = snap.panes.find(p => p.id === 'p1');
    expect(p1).toBeDefined();
    expect(p1!.live).toBe(false);
    // Workspace 1 is DB-known, so p1's membership comes from the DB loop
    // itself (unconditional on live) — not the backfill loop this fix
    // touches — so a natural exit's DB-backed pane is unaffected by it.
    const ws = snap.workspaces.find(w => w.id === 'w1')!;
    expect(ws).toBeDefined();
    expect(ws.paneIds).toContain('p1');
  });

  // Regression repro: an earlier version of the closed-workspace fix skipped
  // every non-live pane in the stub-workspace backfill, not just
  // protocol-closed ones. That silently dropped a naturally-exited,
  // never-persisted pane from its workspace's paneIds — and EVERY legacy /ws
  // pane is never-persisted (handleConnection calls spawnPane but never
  // sessionStore.createPane), so this was the common case, not an edge case,
  // and it contradicted this method's own "every pane must still be reachable
  // via workspaces[].paneIds" invariant (see the comment above the backfill
  // loop in snapshot()). The fix must rely on state.remove() (protocol-close
  // only) for the closed-workspace case, not on a live check here.
  it('a natural PTY exit keeps a never-persisted pane in its DB-known workspace paneIds (regression repro)', () => {
    const rtInstance = createRuntime({ nodeId: 'test-node', spacesDbPath: dbPath });
    // No DB pane row at all for this id — mirrors the legacy /ws path,
    // which spawns via spawnPane/registerPane but never calls
    // sessionStore.createPane. Tagged to workspace 1, which IS DB-known.
    rtInstance.registerPane('natural-p1', {
      cols: 80, rows: 24, agentType: 'shell', cwd: 'C:\\work', workspaceId: 'w1',
      onInput: () => {}, onResize: () => {},
    });
    rtInstance.paneExit('natural-p1', 0); // natural exit, not a protocol close
    const snap = rtInstance.snapshot();
    const pane = snap.panes.find(p => p.id === 'natural-p1');
    expect(pane).toBeDefined();
    expect(pane!.live).toBe(false);
    const ws = snap.workspaces.find(w => w.id === 'w1')!;
    expect(ws).toBeDefined();
    expect(ws.paneIds).toContain('natural-p1'); // pre-fix behavior, restored
  });

  it('a natural PTY exit keeps a never-persisted pane in its own stub workspace (no DB workspace either)', () => {
    const rtInstance = createRuntime({ nodeId: 'n', spacesDbPath: '' });
    rtInstance.registerPane('natural-p2', {
      cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'wZZZ',
      onInput: () => {}, onResize: () => {},
    });
    rtInstance.paneExit('natural-p2', 0);
    const snap = rtInstance.snapshot();
    const ws = snap.workspaces.find(w => w.id === 'wZZZ');
    expect(ws).toBeDefined(); // stub workspace still exists after the exit
    expect(ws!.paneIds).toContain('natural-p2');
    const pane = snap.panes.find(p => p.id === 'natural-p2');
    expect(pane).toBeDefined();
    expect(pane!.live).toBe(false);
  });

  // Confirms the protocol-close case (what finding 4 actually needed) is
  // unaffected by loosening the backfill guard back to a plain
  // `!p.workspaceId` check — it was never the guard's job, only
  // state.remove()'s, and that must hold even for a never-persisted pane
  // (no DB row to delete), not just the DB-backed case already covered in
  // tests/proto/session-control.test.ts.
  it('pane.close still removes a never-persisted pane entirely — from panes and every paneIds', () => {
    const rtInstance = createRuntime({ nodeId: 'test-node', spacesDbPath: dbPath });
    try {
      rtInstance.registerPane('natural-p3', {
        cols: 80, rows: 24, agentType: 'shell', cwd: 'C:\\work', workspaceId: 'w1',
        onInput: () => {}, onResize: () => {},
      });
      // dispatch('pane.close', ...) is this file's first call to touch
      // sessionStore's persistent read-write connection (every other test
      // here only uses registerPane/paneExit/dispatch's read-only methods,
      // which go through index.js's own separate, immediately-closed
      // readonly handle) — close it in a finally, or Windows can't remove
      // dbPath's directory in afterEach's rmSync (open-handle lock).
      const res = rtInstance.dispatch('pane.close', { paneId: 'natural-p3' }) as any;
      expect(res.result.ok).toBe(true);
      const snap = rtInstance.snapshot();
      expect(snap.panes.find(p => p.id === 'natural-p3')).toBeUndefined();
      const ws = snap.workspaces.find(w => w.id === 'w1');
      expect(ws?.paneIds ?? []).not.toContain('natural-p3');
    } finally {
      rtInstance.sessionStore.close();
    }
  });

  it('snapshot does not duplicate paneIds when a live pane matches a DB-known workspace', () => {
    const rt = createRuntime({ nodeId: 'test-node', spacesDbPath: dbPath });
    rt.registerPane('p1', {
      cols: 80, rows: 24, agentType: 'claude', cwd: 'C:\\work', workspaceId: 'w1',
      onInput: () => {}, onResize: () => {},
    });
    const snap = rt.snapshot();
    const ws = snap.workspaces.find(w => w.id === 'w1')!;
    expect(ws.paneIds).toEqual(['p1']);
  });

  it('registering an already-live paneId disposes the previous screen instead of leaking it', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
    const firstScreen = rt.getScreen('p1')!;
    const disposeSpy = vi.spyOn(firstScreen, 'dispose');

    expect(() => rt.registerPane('p1', {
      cols: 100, rows: 30, agentType: 'shell', cwd: '.', workspaceId: 'w1',
      onInput: () => {}, onResize: () => {},
    })).not.toThrow();

    expect(disposeSpy).toHaveBeenCalledTimes(1);
    expect(rt.getScreen('p1')).not.toBe(firstScreen);
  });

  it('dispatch: session.snapshot, pane.read, pane.focus, node.info, unknown method', async () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
    rt.feed('p1', 'grid content');
    await new Promise(r => setTimeout(r, 50));

    expect((rt.dispatch('session.snapshot', {}) as any).result.protocolVersion).toBe(1);
    const read = rt.dispatch('pane.read', { paneId: 'p1', source: 'visible' }) as any;
    expect(read.result.rows.join('')).toContain('grid content');
    expect((rt.dispatch('pane.read', { paneId: 'zzz', source: 'visible' }) as any).error.code).toBe('not_found');
    expect((rt.dispatch('pane.focus', { paneId: 'p1' }) as any).result.ok).toBe(true);
    expect((rt.dispatch('node.info', {}) as any).result.nodeId).toBe('n');
    expect((rt.dispatch('bogus.method', {}) as any).error.code).toBe('bad_request');
  });

  // I1: paneExit must notify attached pane sockets before dropping the
  // record, or an observer sits OPEN forever with no terminal frame and a
  // controller's input is accepted and silently dropped.
  it('paneExit calls each dataSub\'s onExit before dropping the record (I1)', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
    const exits: number[] = [];
    rt.subscribePaneData('p1', () => {}, (exitCode: number) => exits.push(exitCode));
    rt.paneExit('p1', 3);
    expect(exits).toEqual([3]);
    expect(rt.getScreen('p1')).toBeUndefined();
  });

  it('subscribePaneData without an onExit callback is unaffected by paneExit (backward compatible)', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
    rt.subscribePaneData('p1', () => {});
    expect(() => rt.paneExit('p1', 0)).not.toThrow();
  });

  // I5: pane.focus must be exclusive so the done/seen mechanic keeps working
  // after the first focus change.
  it('dispatch pane.focus is exclusive: focusing p2 unfocuses p1 (I5)', () => {
    const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
    rt.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    rt.state.open('p2', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    expect((rt.dispatch('pane.focus', { paneId: 'p1' }) as any).result.ok).toBe(true);
    expect(rt.state.get('p1')!.focused).toBe(true);

    expect((rt.dispatch('pane.focus', { paneId: 'p2' }) as any).result.ok).toBe(true);
    expect(rt.state.get('p1')!.focused).toBe(false);
    expect(rt.state.get('p2')!.focused).toBe(true);

    // p1 is no longer focused — a working->idle completion should now
    // render it "done" (seen=false) instead of being permanently pinned
    // seen=true by a one-way focus.
    rt.state.setAgentState('p1', 'working');
    rt.state.setAgentState('p1', 'idle');
    expect(rt.state.get('p1')!.seen).toBe(false);
  });

  // I6: dispatch must validate params rather than silently coercing garbage
  // (e.g. `params.cols | 0` clamping a bad cols value to 0).
  describe('dispatch param validation (I6)', () => {
    it('pane.input rejects a non-string data and an empty paneId', () => {
      const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
      rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
      expect((rt.dispatch('pane.input', { paneId: 'p1', data: 123 }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.input', { paneId: '', data: 'x' }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.input', { paneId: 'p1', data: 'ok\r' }) as any).result.ok).toBe(true);
    });

    it('pane.resize rejects non-positive-integer cols/rows instead of clamping them', () => {
      const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
      rt.registerPane('p1', { cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: 'w1', onInput: () => {}, onResize: () => {} });
      expect((rt.dispatch('pane.resize', { paneId: 'p1', cols: 0, rows: 24 }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.resize', { paneId: 'p1', cols: 80, rows: -1 }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.resize', { paneId: 'p1', cols: 'nope', rows: 24 }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.resize', { paneId: 'p1', cols: 80.5, rows: 24 }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.resize', { paneId: 'p1', cols: undefined, rows: 24 }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.resize', { paneId: 'p1', cols: 100, rows: 30 }) as any).result.ok).toBe(true);
    });

    it('pane.focus and pane.read reject an empty/missing paneId', () => {
      const rt = createRuntime({ nodeId: 'n', spacesDbPath: dbPath });
      expect((rt.dispatch('pane.focus', { paneId: '' }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.focus', {}) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.read', { paneId: '' }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('pane.read', {}) as any).error.code).toBe('bad_request');
    });
  });
});
