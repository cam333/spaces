// tests/proto/session-control.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import Database from 'better-sqlite3';

// Fault-injection plumbing for the "spawn succeeded but registration was
// silently swallowed" test below (see the `pane.create` describe block).
// index.js is plain CommonJS (`const { spawnPane } = require('./spawn-pane')`)
// and vi.mock cannot reach that nested `require()` call — it only
// intercepts this test file's own top-level imports, not what a CJS
// dependency requires internally. So this uses genuine Node module
// semantics instead of vitest's mock registry: get the *same* cached
// `spawn-pane.js` module object index.js will itself require, and mutate
// its `spawnPane` export in place *before* index.js is required. index.js's
// destructuring assignment then reads our wrapper as the current value of
// that property — reassigning it any later would be too late, since
// destructuring copies the value once, not a live binding.
const require = createRequire(import.meta.url);
const spawnPaneModule = require('../../bin/lib/runtime/spawn-pane.js');
const realSpawnPane = spawnPaneModule.spawnPane;
const registrationFault = { armed: false, killed: false };
// Every call's opts are recorded here (armed or not) so tests can inspect
// exactly what pane.create handed to spawnPane — e.g. the env allowlist
// finding below, without needing its own separate interception scheme.
const spawnCalls: any[] = [];
spawnPaneModule.spawnPane = (opts: any) => {
  spawnCalls.push(opts);
  if (!registrationFault.armed) return realSpawnPane(opts);
  // Stand-in for spawnPane's own documented failure mode: the PTY spawns
  // fine (ok:true) but its internal, best-effort
  // `runtime.registerPane(...)` call threw and was swallowed (see
  // spawn-pane.js's "runtime bookkeeping is best-effort" catch), so `live`
  // never gets an entry for the pane. A `kill()` spy on the returned term
  // proves the fix actually kills the would-be-orphaned process — the real
  // node-pty path used by every other test in this file gives dispatch no
  // way to observe that, since it never returns `term` to the caller.
  registrationFault.killed = false;
  return {
    ok: true,
    launchCommand: '',
    term: {
      kill: () => { registrationFault.killed = true; },
      write() {}, resize() {}, onData() {}, onExit() {},
    },
  };
};
const { createRuntime } = require('../../bin/lib/runtime/index.js');

let dir: string, dbPath: string, wsId: number;

beforeEach(() => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-sc-'));
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
  wsId = Number(db.prepare("INSERT INTO workspaces (name, color) VALUES ('w', null)").run().lastInsertRowid);
  db.close();
});

// Windows cannot remove a directory containing a file with an open handle.
// createSessionStore() keeps its better-sqlite3 connection open for the
// runtime's lifetime by design (see session-store.js) and close() is terminal,
// so it can't be closed mid-dispatch — track every runtime created in a test
// and close its store here, before rmSync, the same way session-store.test.ts
// closes its store before its own cleanup.
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

describe('pane.create', () => {
  it('creates a pane, persists it, and returns a server-generated id', async () => {
    const runtime = rt();
    const res = runtime.dispatch('pane.create', {
      workspaceId: `w${wsId}`, agentType: 'shell', cwd: process.cwd(), title: 'test pane',
    }) as any;
    expect(res.error).toBeUndefined();
    expect(typeof res.result.paneId).toBe('string');
    expect(res.result.paneId.length).toBeGreaterThan(10);
    expect(res.result.agentType).toBe('shell');
    expect(rows('SELECT id FROM panes')).toEqual([{ id: res.result.paneId }]);
    // The pane is live in the runtime and visible in the snapshot.
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.find((p: any) => p.id === res.result.paneId)).toMatchObject({ live: true });
    runtime.dispatch('pane.close', { paneId: res.result.paneId });
    await settle();
  }, 15000);

  it('emits pane.created', async () => {
    const runtime = rt();
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));
    const res = runtime.dispatch('pane.create', { workspaceId: `w${wsId}`, agentType: 'shell', cwd: process.cwd() }) as any;
    expect(events.some(e => e.type === 'pane.created' && e.payload.paneId === res.result.paneId)).toBe(true);
    runtime.dispatch('pane.close', { paneId: res.result.paneId });
    await settle();
  }, 15000);

  it('returns the launch command for an agent pane without requiring the binary', async () => {
    const runtime = rt();
    const res = runtime.dispatch('pane.create', {
      workspaceId: `w${wsId}`, agentType: 'claude', cwd: process.cwd(), agentSession: 'sess9',
    }) as any;
    expect(res.result.launchCommand).toBe('claude --resume sess9');
    runtime.dispatch('pane.close', { paneId: res.result.paneId });
    await settle();
  }, 15000);

  it('rejects a missing or malformed workspaceId', () => {
    const runtime = rt();
    expect((runtime.dispatch('pane.create', {}) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('pane.create', { workspaceId: '' }) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('pane.create', { workspaceId: 'w9999' }) as any).error.code).toBe('not_found');
  });

  it('rejects a non-positive cols/rows', () => {
    const runtime = rt();
    const bad = runtime.dispatch('pane.create', { workspaceId: `w${wsId}`, cols: 0, rows: 24 }) as any;
    expect(bad.error.code).toBe('bad_request');
  });

  it('leaves no orphan row when the spawn fails', () => {
    const runtime = rt();
    // A cwd that cannot exist makes node-pty's spawn fail.
    const res = runtime.dispatch('pane.create', {
      workspaceId: `w${wsId}`, agentType: 'shell', cwd: path.join(dir, 'no', 'such', 'dir'),
    }) as any;
    expect(res.error).toBeDefined();
    expect(rows('SELECT id FROM panes')).toEqual([]);
  });

  it('leaves no orphan row, kills the pty, and does not emit pane.created when spawnPane succeeds but registration silently fails', () => {
    const runtime = rt();
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));
    registrationFault.armed = true;
    let res: any;
    try {
      res = runtime.dispatch('pane.create', { workspaceId: `w${wsId}`, agentType: 'shell', cwd: process.cwd() });
    } finally {
      registrationFault.armed = false;
    }
    expect(res.error).toBeDefined();
    expect(res.error.code).toBe('internal');
    expect(rows('SELECT id FROM panes')).toEqual([]);
    expect(events.some(e => e.type === 'pane.created')).toBe(false);
    expect(registrationFault.killed).toBe(true);
  });

  // Regression test: pane.create previously echoed params.workspaceId raw into
  // the live record, the pane.created event, and the result — so a
  // non-canonical "w0N" input desynced the live record's workspaceId from the
  // canonical form every other method (workspace.close, pane.move,
  // session.snapshot) compares against.
  it('canonicalizes a non-canonical workspaceId in the live record, event, and result', async () => {
    const runtime = rt();
    const nonCanonical = `w0${wsId}`; // e.g. "w01" for wsId=1 — same row, non-canonical spelling
    const canonical = `w${wsId}`;
    const events: any[] = [];
    runtime.hub.subscribe((e: any) => events.push(e));

    const res = runtime.dispatch('pane.create', {
      workspaceId: nonCanonical, agentType: 'shell', cwd: process.cwd(),
    }) as any;
    expect(res.error).toBeUndefined();
    expect(res.result.workspaceId).toBe(canonical);
    const created = events.find(e => e.type === 'pane.created');
    expect(created.payload.workspaceId).toBe(canonical);
    expect(runtime.state.get(res.result.paneId)!.workspaceId).toBe(canonical);

    // Proves the desync is actually fixed, not just the echoed value: a
    // workspace.close using the canonical id must find and kill this pane —
    // previously the live record's workspaceId stayed "w01", so
    // workspace.close's `r.workspaceId === workspaceId` (built from the
    // canonical parse) never matched, leaking the PTY while still reporting
    // it in closedPanes.
    const closeRes = runtime.dispatch('workspace.close', { workspaceId: canonical }) as any;
    expect(closeRes.result.ok).toBe(true);
    expect(closeRes.result.closedPanes).toContain(res.result.paneId);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.find((p: any) => p.id === res.result.paneId)).toBeUndefined();
    await settle();
  }, 15000);

  // Regression test: pane.create passed `env: process.env` straight through to
  // spawnPane, so a protocol-created pane inherited every server secret
  // (SPACES_SESSION_SECRET, GH_PAT, NPM_TOKEN, ANTHROPIC_API_KEY, etc) — unlike
  // the legacy /ws path, which deliberately builds a minimal allowlisted env.
  // Also checks the agent-bus wiring (SPACES_PANE_ID) is present, since panes
  // exist to run agents.
  it('spawns with an allowlisted env — no server secrets, but the agent-bus wiring is present', async () => {
    const runtime = rt();
    process.env.SPACES_SESSION_SECRET_TEST = 'top-secret-value-should-never-leak';
    try {
      spawnCalls.length = 0;
      const res = runtime.dispatch('pane.create', {
        workspaceId: `w${wsId}`, agentType: 'shell', cwd: process.cwd(),
      }) as any;
      expect(res.error).toBeUndefined();
      const call = spawnCalls.at(-1);
      expect(call.env).toBeDefined();
      expect(call.env.SPACES_SESSION_SECRET_TEST).toBeUndefined();
      expect(call.env.SPACES_PANE_ID).toBe(res.result.paneId);
      expect(call.env.SPACES_API_URL).toMatch(/^http:\/\/localhost:\d+$/);
      runtime.dispatch('pane.close', { paneId: res.result.paneId });
      await settle();
    } finally {
      delete process.env.SPACES_SESSION_SECRET_TEST;
    }
  }, 15000);
});

describe('pane.close', () => {
  it('closes a live pane, removes the row, and emits pane.removed', async () => {
    const runtime = rt();
    const events: any[] = [];
    const created = (runtime.dispatch('pane.create', {
      workspaceId: `w${wsId}`, agentType: 'shell', cwd: process.cwd(),
    }) as any).result;
    runtime.hub.subscribe((e: any) => events.push(e));
    const res = runtime.dispatch('pane.close', { paneId: created.paneId }) as any;
    expect(res.result.ok).toBe(true);
    expect(rows('SELECT id FROM panes')).toEqual([]);
    expect(events.some(e => e.type === 'pane.removed' && e.payload.paneId === created.paneId)).toBe(true);
    await settle();
  }, 15000);

  it('reports not_found for an unknown pane and bad_request for a blank id', () => {
    const runtime = rt();
    expect((runtime.dispatch('pane.close', { paneId: 'ghost' }) as any).error.code).toBe('not_found');
    expect((runtime.dispatch('pane.close', {}) as any).error.code).toBe('bad_request');
  });

  // Regression test: previously the pane's state record survived close()
  // forever (live:false), so it kept showing up in every later
  // session.snapshot(). A protocol-initiated close has nothing "recently
  // exited" worth showing — it must vanish entirely.
  it('removes the pane entirely from the next snapshot, not just live:false', async () => {
    const runtime = rt();
    const created = (runtime.dispatch('pane.create', {
      workspaceId: `w${wsId}`, agentType: 'shell', cwd: process.cwd(),
    }) as any).result;
    const res = runtime.dispatch('pane.close', { paneId: created.paneId }) as any;
    expect(res.result.ok).toBe(true);
    const snap = (runtime.dispatch('session.snapshot', {}) as any).result;
    expect(snap.panes.find((p: any) => p.id === created.paneId)).toBeUndefined();
    await settle();
  }, 15000);

  it('actually kills the pty of a pane registered via the legacy spawn path (not just pane.create)', async () => {
    // Regression test: registerPane initialized term:null and only
    // pane.create's own manual `rec.term = spawned.term` ever filled it in, so
    // a pane spawned through the legacy /ws handler (which never runs that
    // manual assignment) had rec.term stuck at null forever — pane.close's
    // `rec.term?.kill()` was a silent no-op for every such pane. spawnPane now
    // threads `term` through its own registerPane call, so this must work for a
    // pane registered WITHOUT ever going through pane.create's dispatch case.
    const runtime = rt();
    let killed = false;
    const fakePtyLib = {
      spawn() {
        return {
          write() {}, resize() {}, onData() {}, onExit() {},
          kill: () => { killed = true; },
        };
      },
    };
    const spawned = realSpawnPane({
      paneId: 'legacy-pane', cwd: process.cwd(), agentType: 'shell', cols: 80, rows: 24,
      env: process.env, runtime, workspaceId: `w${wsId}`, ptyLib: fakePtyLib,
    });
    expect(spawned.ok).toBe(true);
    expect(runtime.state.get('legacy-pane')).toBeDefined();

    const res = runtime.dispatch('pane.close', { paneId: 'legacy-pane' }) as any;
    expect(res.result.ok).toBe(true);
    expect(killed).toBe(true);
  });
});
