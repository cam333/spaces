// tests/detect/notify.test.ts
import { describe, it, expect } from 'vitest';
import { EventHub } from '../../bin/lib/runtime/event-hub.js';
import { PaneStateStore } from '../../bin/lib/runtime/state.js';
import { createNotifier, SIGNALS } from '../../bin/lib/runtime/notify.js';

function harness(delayMs = 1000) {
  const hub = new EventHub();
  const state = new PaneStateStore(hub);
  const delivered: any[] = [];
  let clock = 0;
  const notifier = createNotifier({
    state, deliver: (n: any) => delivered.push(n),
    now: () => clock, delayMs,
  });
  hub.subscribe(e => { if (e.type === 'pane.state') notifier.onStateEvent(e.payload); });
  return {
    hub, state, notifier, delivered,
    advance(ms: number) { clock += ms; notifier.flush(clock); },
  };
}

describe('createNotifier', () => {
  it('delivers needs_attention when a pane becomes blocked', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'blocked');
    h.advance(1001);
    expect(h.delivered).toHaveLength(1);
    expect(h.delivered[0].signal).toBe(SIGNALS.NEEDS_ATTENTION);
    expect(h.delivered[0].paneId).toBe('p1');
    expect(h.delivered[0].agentType).toBe('claude');
  });

  it('delivers finished on an unseen completion', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'codex', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'working');
    h.state.setAgentState('p1', 'idle');
    h.advance(1001);
    expect(h.delivered.map(d => d.signal)).toEqual([SIGNALS.FINISHED]);
  });

  it('does not notify for a completion on a focused pane', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'codex', cwd: '.', workspaceId: null });
    h.state.setExclusiveFocus('p1');
    h.state.setAgentState('p1', 'working');
    h.state.setAgentState('p1', 'idle');
    h.advance(1001);
    expect(h.delivered).toEqual([]);
  });

  it('drops a finished notification if the pane is focused before the delay elapses', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'codex', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'working');
    h.state.setAgentState('p1', 'idle'); // unfocused completion -> finished pending
    h.advance(500);
    expect(h.delivered).toEqual([]); // still within the delay window

    h.state.setExclusiveFocus('p1'); // user looks at it before the delay elapses
    h.advance(600); // now past the 1000ms delay
    expect(h.delivered).toEqual([]);
  });

  it('cancels a pending blocked notification if the pane resolves first', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'blocked');
    h.advance(200);
    h.state.setAgentState('p1', 'working');
    h.advance(2000);
    expect(h.delivered).toEqual([]);
  });

  it('does not deliver before the delay elapses', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'blocked');
    h.advance(999);
    expect(h.delivered).toEqual([]);
    h.advance(2);
    expect(h.delivered).toHaveLength(1);
  });

  it('ignores transitions to working and unknown', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'working');
    h.state.setAgentState('p1', 'unknown');
    h.advance(5000);
    expect(h.delivered).toEqual([]);
  });

  it('forget cancels a pending notification for a closed pane', () => {
    const h = harness();
    h.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: null });
    h.state.setAgentState('p1', 'blocked');
    h.notifier.forget('p1');
    h.advance(5000);
    expect(h.delivered).toEqual([]);
  });

  it('a deliver sink that throws does not break later notifications', () => {
    const hub = new EventHub();
    const state = new PaneStateStore(hub);
    let clock = 0;
    const seen: string[] = [];
    let first = true;
    const notifier = createNotifier({
      state,
      deliver: (n: any) => { if (first) { first = false; throw new Error('sink down'); } seen.push(n.paneId); },
      now: () => clock, delayMs: 10,
    });
    hub.subscribe(e => { if (e.type === 'pane.state') notifier.onStateEvent(e.payload); });
    state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: null });
    state.open('p2', { agentType: 'claude', cwd: '.', workspaceId: null });
    state.setAgentState('p1', 'blocked');
    state.setAgentState('p2', 'blocked');
    clock = 100;
    expect(() => notifier.flush(clock)).not.toThrow();
    expect(seen).toEqual(['p2']);
  });
});
