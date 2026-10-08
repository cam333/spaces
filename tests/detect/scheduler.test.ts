import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRegistry } from '../../bin/lib/runtime/detect/registry.js';
import { createScheduler } from '../../bin/lib/runtime/detect/scheduler.js';
import { EventHub } from '../../bin/lib/runtime/event-hub.js';
import { PaneStateStore } from '../../bin/lib/runtime/state.js';

let bundledDir: string;

beforeEach(() => {
  bundledDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-sched-'));
  fs.writeFileSync(path.join(bundledDir, 'demo.json'), JSON.stringify({
    agent: 'demo', version: '1', minEngineVersion: 1,
    rules: [
      { id: 'busy', state: 'working', priority: 900, region: 'whole_visible', contains: ['working…'] },
      { id: 'ask', state: 'blocked', priority: 950, region: 'whole_visible', contains: ['proceed?'] },
      { id: 'ready', state: 'idle', priority: 500, region: 'whole_visible', lineRegex: ['^\\$ $'] },
    ],
  }));
});

afterEach(() => fs.rmSync(bundledDir, { recursive: true, force: true }));

function harness() {
  const hub = new EventHub();
  const state = new PaneStateStore(hub);
  const registry = createRegistry({ bundledDir });
  const screens = new Map<string, { rows: string[] }>();
  const getScreen = (paneId: string) => {
    const s = screens.get(paneId);
    if (!s) return undefined;
    return { detectionSnapshot: () => ({ rows: s.rows, title: '', progress: null, cols: 80, rows_count: s.rows.length }) };
  };
  let clock = 0;
  const scheduler = createScheduler({
    registry, getScreen, state,
    now: () => clock,
    setIntervalFn: () => 1, clearIntervalFn: () => {},
  });
  const setScreen = (paneId: string, rows: string[]) => {
    screens.set(paneId, { rows });
    scheduler.markDirty(paneId);
  };
  const advance = (ms: number) => { clock += ms; };
  return { hub, state, scheduler, setScreen, advance };
}

describe('detection scheduler', () => {
  it('sets working when the manifest matches', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']);
    h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working');
  });

  it('sets blocked immediately from working', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    h.setScreen('p1', ['do you want to proceed?']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('blocked');
  });

  it('debounces working -> idle across ticks and then marks done', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working');
    for (let i = 0; i < 2; i++) { h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick(); }
    expect(h.state.get('p1')!.agentState).toBe('working'); // still held
    h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick();
    const rec = h.state.get('p1')!;
    expect(rec.agentState).toBe('idle');
    expect(rec.seen).toBe(false); // unfocused completion == done
  });

  it('skips state updates for a skipStateUpdate rule', () => {
    fs.writeFileSync(path.join(bundledDir, 'demo.json'), JSON.stringify({
      agent: 'demo', version: '2', minEngineVersion: 1,
      rules: [
        { id: 'busy', state: 'working', priority: 900, region: 'whole_visible', contains: ['working…'] },
        { id: 'viewer', state: 'unknown', priority: 1000, region: 'whole_visible', contains: ['transcript'], skipStateUpdate: true },
      ],
    }));
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working');
    h.setScreen('p1', ['transcript view']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working'); // frozen, not changed
  });

  it('leaves state unknown for an agent with no manifest', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'shell', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'shell');
    h.setScreen('p1', ['$ ']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('unknown');
    expect(h.scheduler.lastResult('p1')!.reason).toBe('no_manifest');
  });

  it('only evaluates dirty panes', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    const first = h.scheduler.lastResult('p1')!.at;
    h.advance(100);
    h.scheduler.tick(); // no markDirty since last tick
    expect(h.scheduler.lastResult('p1')!.at).toBe(first);
  });

  it('lastResult carries manifest source and version', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    const r = h.scheduler.lastResult('p1')!;
    expect(r.manifestSource).toBe('bundled');
    expect(r.manifestVersion).toBe('1');
    expect(r.ruleId).toBe('busy');
  });

  it('untrack stops evaluating and clears the result', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    h.scheduler.untrack('p1');
    expect(h.scheduler.lastResult('p1')).toBeNull();
    h.setScreen('p1', ['do you want to proceed?']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working'); // unchanged
  });

  it('survives a screen that throws', () => {
    const hub = new EventHub();
    const state = new PaneStateStore(hub);
    const registry = createRegistry({ bundledDir });
    const scheduler = createScheduler({
      registry,
      getScreen: () => ({ detectionSnapshot: () => { throw new Error('disposed'); } }),
      state, now: () => 0, setIntervalFn: () => 1, clearIntervalFn: () => {},
    });
    state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    scheduler.track('p1', 'demo');
    scheduler.markDirty('p1');
    expect(() => scheduler.tick()).not.toThrow();
    expect(state.get('p1')!.agentState).toBe('unknown');
  });

  it('survives an evaluate() that throws, recording the failure instead of crashing the tick', () => {
    // Defence in depth for evaluate()'s never-throws contract: this runs
    // inside a bare setInterval callback (production `tick`), so an
    // uncaught throw here would kill the process. A getter that throws on
    // access reproduces a genuine throw from evaluate() itself without
    // needing to mock the module.
    const hub = new EventHub();
    const state = new PaneStateStore(hub);
    const badManifest = {
      agent: 'demo', version: '1',
      get rules() { throw new Error('boom'); },
    };
    const registry = { get: () => badManifest, sourceOf: () => 'test' };
    const scheduler = createScheduler({
      registry,
      getScreen: () => ({
        detectionSnapshot: () => ({ rows: ['hi'], title: '', progress: null, cols: 80, rows_count: 1 }),
      }),
      state, now: () => 0, setIntervalFn: () => 1, clearIntervalFn: () => {},
    });
    state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    scheduler.track('p1', 'demo');
    scheduler.markDirty('p1');
    expect(() => scheduler.tick()).not.toThrow();
    expect(state.get('p1')!.agentState).toBe('unknown'); // untouched by the failed evaluation
    expect(scheduler.lastResult('p1')!.reason).toContain('evaluate_threw');
  });

  it('re-tracking clears stale debounce state so the new session requires its own confirmations', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working');

    // Bank one debounce confirmation toward idle before re-tracking.
    h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick();
    expect(h.scheduler.pendingIdle('p1')).not.toBeNull();
    expect(h.state.get('p1')!.agentState).toBe('working'); // still held

    // Re-track without an intervening untrack (mirrors registerPane's
    // re-register path in bin/lib/runtime/index.js, which disposes the old
    // screen but must not let the new session inherit old debounce credit).
    h.scheduler.track('p1', 'demo');
    expect(h.scheduler.pendingIdle('p1')).toBeNull();

    // A single new-session idle observation, even far past IDLE_CAP_MS, must
    // NOT inherit the old session's banked confirmation and instantly commit.
    h.setScreen('p1', ['$ ']); h.advance(800); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working'); // still held

    // The new session still needs its own IDLE_CONFIRMATIONS (3) worth of
    // observations before it actually commits idle.
    h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick();
    h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('idle');
  });

  it('resolves an open idle hold within the cap even with no further PTY output', () => {
    // Reproduces the completion-detection stall: an agent's final render
    // arrives in one burst (one markDirty) and then the PTY goes quiet.
    // markDirty is the only production entry point into `dirty`, so once
    // the debouncer has opened a hold, nothing else will ever re-dirty the
    // pane — the scheduler itself must keep re-evaluating until the hold
    // resolves.
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working');

    // One dirty mark, then silence: no further setScreen/markDirty calls.
    h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick();
    expect(h.scheduler.pendingIdle('p1')).not.toBeNull();
    expect(h.state.get('p1')!.agentState).toBe('working'); // still held

    // Ticks fire on their own clock (every 300ms in production) with no
    // intervening PTY output — the scheduler must self-sustain the pane in
    // `dirty` until the hold resolves, within IDLE_CONFIRMATIONS (3) ticks.
    h.advance(300); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('working'); // still held
    h.advance(300); h.scheduler.tick();

    expect(h.state.get('p1')!.agentState).toBe('idle');
  });

  it('stops self re-dirtying once the idle hold resolves', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    h.setScreen('p1', ['$ ']); h.advance(10); h.scheduler.tick();
    h.advance(300); h.scheduler.tick();
    h.advance(300); h.scheduler.tick();
    expect(h.state.get('p1')!.agentState).toBe('idle');

    const at = h.scheduler.lastResult('p1')!.at;
    // No markDirty since the hold resolved — if the pane stayed dirty
    // forever, this tick would still re-evaluate and bump `at`.
    h.advance(1000); h.scheduler.tick();
    expect(h.scheduler.lastResult('p1')!.at).toBe(at);
  });

  it('lastResult is null immediately after a re-track, before the next tick', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'demo', cwd: '.', workspaceId: null });
    h.scheduler.track('p1', 'demo');
    h.setScreen('p1', ['working…']); h.scheduler.tick();
    expect(h.scheduler.lastResult('p1')).not.toBeNull();

    h.scheduler.track('p1', 'demo');
    expect(h.scheduler.lastResult('p1')).toBeNull();
  });
});
