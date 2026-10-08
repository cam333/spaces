import { describe, it, expect } from 'vitest';
import { EventHub } from '../../bin/lib/runtime/event-hub.js';
import { PaneStateStore } from '../../bin/lib/runtime/state.js';

function setup() {
  const hub = new EventHub();
  const store = new PaneStateStore(hub);
  const events: Array<{ type: string; payload: any }> = [];
  hub.subscribe(e => events.push({ type: e.type, payload: e.payload }));
  return { hub, store, events };
}

describe('PaneStateStore', () => {
  it('open creates a record with defaults and emits pane.opened', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: 'C:\\work', workspaceId: 'w1' });
    const rec = store.get('p1')!;
    expect(rec.agentState).toBe('unknown');
    expect(rec.seen).toBe(true);
    expect(rec.live).toBe(true);
    expect(events[0].type).toBe('pane.opened');
  });

  it('completion transition on unfocused pane clears seen (renders as done)', () => {
    const { store } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.setAgentState('p1', 'working');
    store.setAgentState('p1', 'idle');
    expect(store.get('p1')!.seen).toBe(false); // done
  });

  it('completion transition on focused pane keeps seen', () => {
    const { store } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.setFocused('p1', true);
    store.setAgentState('p1', 'working');
    store.setAgentState('p1', 'idle');
    expect(store.get('p1')!.seen).toBe(true);
  });

  it('blocked always marks unseen work seen=true (attention is not "done")', () => {
    const { store } = setup();
    store.open('p1', { agentType: 'codex', cwd: '.', workspaceId: 'w1' });
    store.setAgentState('p1', 'blocked');
    expect(store.get('p1')!.agentState).toBe('blocked');
    expect(store.get('p1')!.seen).toBe(true);
  });

  it('focusing later marks a done pane seen and emits pane.seen', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.setAgentState('p1', 'working');
    store.setAgentState('p1', 'idle');
    events.length = 0;
    store.setFocused('p1', true);
    expect(store.get('p1')!.seen).toBe(true);
    expect(events.some(e => e.type === 'pane.seen')).toBe(true);
  });

  it('idempotent setAgentState emits nothing', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    events.length = 0;
    store.setAgentState('p1', 'unknown');
    expect(events).toEqual([]);
  });

  it('rollup priority: blocked > working > done > idle > unknown', () => {
    const mk = (agentState: string, seen: boolean) => ({ agentState, seen });
    expect(PaneStateStore.rollup([mk('idle', true), mk('blocked', true)])).toBe('blocked');
    expect(PaneStateStore.rollup([mk('idle', true), mk('working', true)])).toBe('working');
    expect(PaneStateStore.rollup([mk('idle', true), mk('idle', false)])).toBe('done');
    expect(PaneStateStore.rollup([mk('idle', true), mk('unknown', true)])).toBe('idle');
    expect(PaneStateStore.rollup([mk('unknown', true)])).toBe('unknown');
    expect(PaneStateStore.rollup([])).toBe('unknown');
  });

  it('close marks live=false and emits pane.closed with exitCode', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'shell', cwd: '.', workspaceId: 'w1' });
    events.length = 0;
    store.close('p1', 0);
    expect(store.get('p1')!.live).toBe(false);
    expect(events[0]).toEqual({ type: 'pane.closed', payload: { paneId: 'p1', exitCode: 0 } });
  });

  it('setSessionRef only emits on change (idempotent)', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    events.length = 0;
    store.setSessionRef('p1', { id: 's1', transcriptPath: '/path/to/transcript' });
    expect(events.length).toBe(1);
    expect(events[0].type).toBe('pane.session');
    events.length = 0;
    store.setSessionRef('p1', { id: 's1', transcriptPath: '/path/to/transcript' });
    expect(events).toEqual([]); // idempotent, no emit
    events.length = 0;
    store.setSessionRef('p1', { id: 's2', transcriptPath: '/path/to/transcript' });
    expect(events.length).toBe(1); // changed id, emits
  });

  it('re-opening a live pane is a no-op, preserving existing state', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.setTitle('p1', 'My Pane');
    store.setFocused('p1', true);
    events.length = 0;
    store.open('p1', { agentType: 'different', cwd: '/other', workspaceId: 'w2' });
    expect(events).toEqual([]); // no pane.opened
    const rec = store.get('p1')!;
    expect(rec.title).toBe('My Pane'); // preserved
    expect(rec.focused).toBe(true); // preserved
    expect(rec.agentType).toBe('claude'); // not changed
  });

  it('setExclusiveFocus unfocuses every other pane; a completion on the newly-unfocused pane renders done (I5)', () => {
    const { store } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.open('p2', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });

    store.setExclusiveFocus('p1');
    expect(store.get('p1')!.focused).toBe(true);
    expect(store.get('p2')!.focused).toBe(false);

    store.setExclusiveFocus('p2');
    expect(store.get('p1')!.focused).toBe(false); // p1 unfocused
    expect(store.get('p2')!.focused).toBe(true);

    // Without I5, p1 stayed focused forever (setFocused(id, false) was
    // never called anywhere), so this completion would keep seen=true —
    // p1 could never again render as "done".
    store.setAgentState('p1', 'working');
    store.setAgentState('p1', 'idle');
    expect(store.get('p1')!.seen).toBe(false);
  });

  it('setExclusiveFocus on an unknown paneId is a safe no-op', () => {
    const { store } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.setExclusiveFocus('p1');
    expect(() => store.setExclusiveFocus('nope')).not.toThrow();
    expect(store.get('p1')!.focused).toBe(true); // unchanged
  });

  it('remove drops the record entirely, unlike close (which keeps a live:false record)', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.close('p1', 0);
    expect(store.get('p1')).toBeDefined(); // close() keeps it around
    events.length = 0;
    store.remove('p1');
    expect(store.get('p1')).toBeUndefined();
    expect(events).toEqual([]); // no event — the caller's own pane.removed covers it
    expect(store.snapshotPanes().find(r => r.paneId === 'p1')).toBeUndefined();
  });

  it('remove on an unknown paneId is a safe no-op', () => {
    const { store } = setup();
    expect(() => store.remove('nope')).not.toThrow();
  });

  it('re-opening after close creates a fresh record', () => {
    const { store, events } = setup();
    store.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    store.setTitle('p1', 'Old Title');
    store.close('p1', 0);
    events.length = 0;
    store.open('p1', { agentType: 'different', cwd: '/other', workspaceId: 'w2' });
    expect(events[0].type).toBe('pane.opened');
    const rec = store.get('p1')!;
    expect(rec.title).toBe(''); // fresh record
    expect(rec.agentType).toBe('different'); // updated
    expect(rec.live).toBe(true);
  });
});
