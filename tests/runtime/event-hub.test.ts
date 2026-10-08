import { describe, it, expect, vi } from 'vitest';
import { EventHub, RING_CAPACITY } from '../../bin/lib/runtime/event-hub.js';

describe('EventHub', () => {
  it('assigns monotonically increasing seqs starting at 1', () => {
    const hub = new EventHub();
    expect(hub.emit('pane.opened', { paneId: 'p1' })).toBe(1);
    expect(hub.emit('pane.state', { paneId: 'p1' })).toBe(2);
    expect(hub.lastSeq).toBe(2);
  });

  it('eventsAfter(0) returns everything with no gap', () => {
    const hub = new EventHub();
    hub.emit('a', {});
    hub.emit('b', {});
    const { gap, events } = hub.eventsAfter(0);
    expect(gap).toBe(false);
    expect(events.map(e => e.seq)).toEqual([1, 2]);
    expect(events[0].type).toBe('a');
  });

  it('eventsAfter(n) returns only events with seq > n', () => {
    const hub = new EventHub();
    hub.emit('a', {}); hub.emit('b', {}); hub.emit('c', {});
    const { gap, events } = hub.eventsAfter(2);
    expect(gap).toBe(false);
    expect(events.map(e => e.seq)).toEqual([3]);
  });

  it('evicts oldest events beyond capacity and reports a gap', () => {
    const hub = new EventHub(3); // small capacity for the test
    for (let i = 0; i < 5; i++) hub.emit('e', { i });
    // buffer now holds seqs [3,4,5]
    expect(hub.eventsAfter(2).gap).toBe(false);
    expect(hub.eventsAfter(2).events.map(e => e.seq)).toEqual([3, 4, 5]);
    expect(hub.eventsAfter(1).gap).toBe(true); // seq 2 is lost
    expect(hub.eventsAfter(0).gap).toBe(true);
  });

  it('notifies live subscribers and survives a throwing listener', () => {
    const hub = new EventHub();
    const seen: number[] = [];
    hub.subscribe(() => { throw new Error('bad listener'); });
    const unsub = hub.subscribe(e => seen.push(e.seq));
    hub.emit('a', {});
    unsub();
    hub.emit('b', {});
    expect(seen).toEqual([1]);
  });
});
