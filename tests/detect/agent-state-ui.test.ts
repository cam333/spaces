import { describe, it, expect } from 'vitest';
import { displayState, rollupState, stateLabel, stateTone, STATE_PRIORITY } from '../../src/lib/proto/agent-state';

const pane = (agentState: string, seen: boolean) => ({ agentState, seen });

describe('displayState', () => {
  it('maps an unseen idle pane to done', () => {
    expect(displayState(pane('idle', false))).toBe('done');
  });
  it('maps a seen idle pane to idle', () => {
    expect(displayState(pane('idle', true))).toBe('idle');
  });
  it('passes through blocked, working and unknown regardless of seen', () => {
    expect(displayState(pane('blocked', false))).toBe('blocked');
    expect(displayState(pane('working', true))).toBe('working');
    expect(displayState(pane('unknown', false))).toBe('unknown');
  });
  it('gracefully handles null pane', () => {
    expect(displayState(null)).toBe('unknown');
  });
  it('gracefully handles undefined pane', () => {
    expect(displayState(undefined)).toBe('unknown');
  });
  it('gracefully handles empty object', () => {
    expect(displayState({})).toBe('unknown');
  });
  it('gracefully handles non-object', () => {
    expect(displayState('nope')).toBe('unknown');
  });
  it('treats undefined seen as falsy (done)', () => {
    expect(displayState({ agentState: 'idle', seen: undefined })).toBe('done');
  });
  it('treats null seen as falsy (done)', () => {
    expect(displayState({ agentState: 'idle', seen: null })).toBe('done');
  });
  it('treats 0 seen as falsy (done)', () => {
    expect(displayState({ agentState: 'idle', seen: 0 })).toBe('done');
  });
  it('returns unknown for unrecognized agentState', () => {
    expect(displayState({ agentState: 'zzz', seen: true })).toBe('unknown');
  });
});

describe('rollupState', () => {
  it('prefers blocked over everything', () => {
    expect(rollupState([pane('idle', false), pane('working', true), pane('blocked', true)])).toBe('blocked');
  });
  it('prefers working over done', () => {
    expect(rollupState([pane('idle', false), pane('working', true)])).toBe('working');
  });
  it('prefers done over idle', () => {
    expect(rollupState([pane('idle', true), pane('idle', false)])).toBe('done');
  });
  it('returns unknown for an empty list', () => {
    expect(rollupState([])).toBe('unknown');
  });
  it('orders priorities blocked > working > done > idle > unknown', () => {
    expect(STATE_PRIORITY.blocked).toBeGreaterThan(STATE_PRIORITY.working);
    expect(STATE_PRIORITY.working).toBeGreaterThan(STATE_PRIORITY.done);
    expect(STATE_PRIORITY.done).toBeGreaterThan(STATE_PRIORITY.idle);
    expect(STATE_PRIORITY.idle).toBeGreaterThan(STATE_PRIORITY.unknown);
  });
  it('skips null entries without throwing', () => {
    expect(rollupState([pane('working', true), null, undefined])).toBe('working');
  });
  it('returns unknown for list with only null entries', () => {
    expect(rollupState([null])).toBe('unknown');
  });
});

describe('presentation helpers', () => {
  it('labels every display state', () => {
    expect(stateLabel('blocked')).toBe('Needs you');
    expect(stateLabel('working')).toBe('Working');
    expect(stateLabel('done')).toBe('Done');
    expect(stateLabel('idle')).toBe('Idle');
    expect(stateLabel('unknown')).toBe('—');
  });
  it('assigns a tone per display state', () => {
    expect(stateTone('blocked')).toBe('danger');
    expect(stateTone('working')).toBe('active');
    expect(stateTone('done')).toBe('success');
    expect(stateTone('idle')).toBe('muted');
    expect(stateTone('unknown')).toBe('muted');
  });
});
