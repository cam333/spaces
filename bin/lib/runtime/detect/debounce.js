'use strict';

const IDLE_CONFIRMATIONS = 3;
const IDLE_CAP_MS = 700;

/**
 * Anti-flap for the working|blocked -> idle transition only: agents blank
 * their status line for a frame or two mid-turn, and committing idle there
 * would fire a spurious "done". Every other transition commits immediately —
 * blocked and working are the states a human is waiting on.
 * Time is injected so the machine stays pure and testable.
 */
function createDebouncer() {
  const pending = new Map(); // paneId -> { count, since }

  function step(paneId, currentState, observedState, now) {
    if (observedState === currentState) {
      pending.delete(paneId);
      return null;
    }
    const isCompletion = observedState === 'idle'
      && (currentState === 'working' || currentState === 'blocked');
    if (!isCompletion) {
      pending.delete(paneId);
      return observedState;
    }
    const entry = pending.get(paneId);
    if (!entry) {
      pending.set(paneId, { count: 1, since: now });
      return null;
    }
    entry.count += 1;
    if (entry.count >= IDLE_CONFIRMATIONS || now - entry.since >= IDLE_CAP_MS) {
      pending.delete(paneId);
      return 'idle';
    }
    return null;
  }

  return {
    step,
    forget(paneId) { pending.delete(paneId); },
    pending(paneId) {
      const e = pending.get(paneId);
      return e ? { count: e.count, since: e.since } : null;
    },
  };
}

module.exports = { createDebouncer, IDLE_CONFIRMATIONS, IDLE_CAP_MS };
