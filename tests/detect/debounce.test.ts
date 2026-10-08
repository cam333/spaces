import { describe, it, expect } from 'vitest';
import { createDebouncer, IDLE_CONFIRMATIONS, IDLE_CAP_MS } from '../../bin/lib/runtime/detect/debounce.js';

describe('createDebouncer', () => {
  it('returns null when the observation matches current state', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'working', 'working', 1000)).toBeNull();
  });

  it('commits a transition to working immediately', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'idle', 'working', 1000)).toBe('working');
  });

  it('commits a transition to blocked immediately', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'working', 'blocked', 1000)).toBe('blocked');
  });

  it('commits a transition to unknown immediately', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'working', 'unknown', 1000)).toBe('unknown');
  });

  it('holds working -> idle until three confirmations', () => {
    const d = createDebouncer();
    expect(IDLE_CONFIRMATIONS).toBe(3);
    expect(d.step('p1', 'working', 'idle', 1000)).toBeNull();
    expect(d.step('p1', 'working', 'idle', 1050)).toBeNull();
    expect(d.step('p1', 'working', 'idle', 1100)).toBe('idle');
  });

  it('commits idle early when the time cap elapses', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'working', 'idle', 1000)).toBeNull();
    expect(d.step('p1', 'working', 'idle', 1000 + IDLE_CAP_MS + 1)).toBe('idle');
  });

  it('cancels a pending idle when a non-idle observation arrives', () => {
    const d = createDebouncer();
    d.step('p1', 'working', 'idle', 1000);
    d.step('p1', 'working', 'idle', 1050);
    expect(d.step('p1', 'working', 'working', 1100)).toBeNull(); // same as current, no commit
    expect(d.pending('p1')).toBeNull();
    // counting restarts from scratch
    expect(d.step('p1', 'working', 'idle', 1200)).toBeNull();
    expect(d.step('p1', 'working', 'idle', 1250)).toBeNull();
    expect(d.step('p1', 'working', 'idle', 1300)).toBe('idle');
  });

  it('commits blocked -> idle through the same hold', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'blocked', 'idle', 1000)).toBeNull();
    expect(d.step('p1', 'blocked', 'idle', 1010)).toBeNull();
    expect(d.step('p1', 'blocked', 'idle', 1020)).toBe('idle');
  });

  it('commits unknown -> idle immediately (not a completion transition)', () => {
    const d = createDebouncer();
    expect(d.step('p1', 'unknown', 'idle', 1000)).toBe('idle');
  });

  it('tracks panes independently', () => {
    const d = createDebouncer();
    d.step('p1', 'working', 'idle', 1000);
    expect(d.step('p2', 'working', 'idle', 1000)).toBeNull();
    expect(d.pending('p1')!.count).toBe(1);
    expect(d.pending('p2')!.count).toBe(1);
  });

  it('forget clears pending state', () => {
    const d = createDebouncer();
    d.step('p1', 'working', 'idle', 1000);
    d.forget('p1');
    expect(d.pending('p1')).toBeNull();
  });
});
