// tests/proto/agent-wait.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';

let dir: string, dbPath: string, runtime: any;

beforeEach(() => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-aw-'));
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
  runtime = createRuntime({ nodeId: 'test', spacesDbPath: dbPath });
});

afterEach(() => fs.rmSync(dir, { recursive: true, force: true }));

describe('awaitAgentState', () => {
  it('resolves immediately when the pane is already in a target state', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    runtime.state.setAgentState('p1', 'blocked');
    const res = await runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 1000 });
    expect(res).toMatchObject({ ok: true, state: 'blocked' });
  });

  it('resolves when a matching state arrives later', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const pending = runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 3000 });
    setTimeout(() => runtime.state.setAgentState('p1', 'blocked'), 50);
    expect(await pending).toMatchObject({ ok: true, state: 'blocked' });
  });

  it('ignores non-matching states while waiting', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const pending = runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 3000 });
    setTimeout(() => runtime.state.setAgentState('p1', 'working'), 30);
    setTimeout(() => runtime.state.setAgentState('p1', 'blocked'), 90);
    expect(await pending).toMatchObject({ ok: true, state: 'blocked' });
  });

  it('ignores other panes while waiting', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    runtime.state.open('p2', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const pending = runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 600 });
    runtime.state.setAgentState('p2', 'blocked');
    const res = await pending;
    expect(res).toMatchObject({ ok: false, code: 'timeout' });
  });

  it('times out with a timeout code', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const res = await runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 150 });
    expect(res).toMatchObject({ ok: false, code: 'timeout' });
  });

  it('rejects bad params without hanging', async () => {
    await expect(runtime.awaitAgentState({ until: ['blocked'] })).resolves.toMatchObject({ ok: false, code: 'bad_request' });
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    await expect(runtime.awaitAgentState({ paneId: 'p1', until: [] })).resolves.toMatchObject({ ok: false, code: 'bad_request' });
    await expect(runtime.awaitAgentState({ paneId: 'p1', until: ['bogus'] })).resolves.toMatchObject({ ok: false, code: 'bad_request' });
    await expect(runtime.awaitAgentState({ paneId: 'ghost', until: ['idle'] })).resolves.toMatchObject({ ok: false, code: 'not_found' });
  });

  // Regression test: awaitAgentState once watched only pane.state, so a
  // pane closed mid-wait (pane.close/workspace.close, or a natural exit)
  // burned the full timeout budget before reporting a misleading `timeout`
  // instead of the pane simply being gone.
  it('resolves not_found promptly when the pane closes mid-wait, instead of burning the full timeout', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const start = Date.now();
    const pending = runtime.awaitAgentState({ paneId: 'p1', until: ['idle'], timeoutMs: 3000 });
    setTimeout(() => runtime.state.close('p1', 0), 50);
    const res = await pending;
    expect(res).toMatchObject({ ok: false, code: 'not_found' });
    expect(Date.now() - start).toBeLessThan(1000); // well under the 3000ms budget
  });

  it('leaks no hub listeners', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const before = runtime.hub._listeners.size;
    await runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 100 });
    const pending = runtime.awaitAgentState({ paneId: 'p1', until: ['idle'], timeoutMs: 3000 });
    runtime.state.setAgentState('p1', 'idle');
    await pending;
    expect(runtime.hub._listeners.size).toBe(before);
  });

  // Regression test: the old implementation scheduled the timer BEFORE calling
  // hub.subscribe, with no try/catch around either. If subscribe threw, the
  // promise rejected (violating "never rejects") AND the already-scheduled
  // timer's callback closed over an `unsubscribe` binding that was never
  // assigned — so ~timeoutMs later it threw a bare, uncaught ReferenceError
  // with no process-level handler to catch it, crashing the whole server. The
  // fix subscribes first inside a try/catch, resolving
  // {ok:false, code:'internal'} on any throw, with `unsubscribe`
  // pre-initialized to a no-op so no later timer callback can ever reference
  // an uninitialized binding.
  it('resolves ok:false — never rejects, never crashes later — when hub.subscribe throws', async () => {
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const before = runtime.hub._listeners.size;
    const realSubscribe = runtime.hub.subscribe.bind(runtime.hub);
    runtime.hub.subscribe = () => { throw new Error('subscribe exploded'); };
    const surfaced: unknown[] = [];
    const onUncaught = (e: unknown) => surfaced.push(e);
    process.on('uncaughtException', onUncaught);
    process.on('unhandledRejection', onUncaught);
    try {
      const res = await runtime.awaitAgentState({ paneId: 'p1', until: ['blocked'], timeoutMs: 150 });
      expect(res).toMatchObject({ ok: false, code: 'internal' });
      // The old bug's timer would have fired ~150ms after the call above —
      // wait past that window and confirm nothing surfaces as an uncaught
      // exception or unhandled rejection (the timer must never even have
      // been scheduled, since subscribe never succeeded).
      await new Promise(r => setTimeout(r, 250));
      expect(surfaced).toEqual([]);
    } finally {
      process.off('uncaughtException', onUncaught);
      process.off('unhandledRejection', onUncaught);
      runtime.hub.subscribe = realSubscribe;
    }
    expect(runtime.hub._listeners.size).toBe(before);
  });
});

describe('promptAndWait', () => {
  // Regression test: the old implementation called writePane(...) unguarded,
  // after `settled` was already constructed. If the pane's onInput threw
  // synchronously (writePane just calls it directly), the exception escaped
  // promptAndWait() itself — before it ever returned a Promise — so it was only
  // catchable by an ordinary try/catch, not `.catch()`. The control channel has
  // no try/catch around this call, so it would have propagated out of the WS
  // message handler and crashed the process. The fix wraps the write in
  // try/catch and settles the existing `settled` promise via the same
  // finish/unsubscribe path as every other exit.
  it('resolves ok:false without throwing synchronously when the pane write throws', async () => {
    runtime.registerPane('p9', {
      cols: 80, rows: 24, agentType: 'claude', cwd: '.', workspaceId: 'w1',
      onInput: () => { throw new Error('pty write exploded'); },
      onResize: () => {},
    });
    const before = runtime.hub._listeners.size;
    let threwSynchronously = false;
    let res: any;
    try {
      res = await runtime.promptAndWait({ paneId: 'p9', text: 'hi', timeoutMs: 500 });
    } catch {
      threwSynchronously = true;
    }
    expect(threwSynchronously).toBe(false);
    expect(res).toMatchObject({ ok: false, code: 'internal' });
    expect(runtime.hub._listeners.size).toBe(before);
  });
});

describe('dispatch guards for the async methods', () => {
  it('tells a synchronous caller to use the control channel', () => {
    const a = runtime.dispatch('agent.wait', { paneId: 'p1', until: ['idle'] }) as any;
    const b = runtime.dispatch('agent.prompt-wait', { paneId: 'p1', text: 'hi' }) as any;
    expect(a.error.code).toBe('bad_request');
    expect(a.error.message).toMatch(/asynchronous/i);
    expect(b.error.code).toBe('bad_request');
  });
});
