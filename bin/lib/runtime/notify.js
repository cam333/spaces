'use strict';

const SIGNALS = { NEEDS_ATTENTION: 'needs_attention', FINISHED: 'finished' };
const DEFAULT_DELAY_MS = 1200;

/**
 * Maps pane state changes to user-facing signals, deferred briefly so a
 * blocked-then-immediately-resolved flicker never reaches the human.
 * `deliver` is injected: the TUI plays a sound, the web posts a Notification.
 */
function createNotifier({ state, deliver, now = () => Date.now(), delayMs = DEFAULT_DELAY_MS } = {}) {
  const pendingByPane = new Map(); // paneId -> { signal, dueAt }

  function signalFor(payload) {
    if (payload.agentState === 'blocked') return SIGNALS.NEEDS_ATTENTION;
    if (payload.agentState === 'idle' && payload.seen === false) return SIGNALS.FINISHED;
    return null;
  }

  function onStateEvent(payload) {
    const signal = signalFor(payload);
    if (!signal) {
      // The pane left a signalling state — drop any deferred notification.
      pendingByPane.delete(payload.paneId);
      return;
    }
    pendingByPane.set(payload.paneId, { signal, dueAt: now() + delayMs });
  }

  function flush(at = now()) {
    for (const [paneId, entry] of [...pendingByPane]) {
      if (entry.dueAt > at) continue;
      pendingByPane.delete(paneId);
      const rec = state.get(paneId);
      if (!rec) continue;
      // The `finished` signal was decided at completion time from that
      // moment's `seen` flag, but the human can focus the pane during the
      // delay window — focusing calls markSeen, which only emits
      // `pane.seen` (this notifier subscribes to `pane.state`, not that),
      // so without rechecking here a stale `finished` would still fire for
      // a pane the user is already looking at. `needs_attention` is left
      // alone: a pane that's still blocked is worth surfacing regardless of
      // focus.
      if (entry.signal === SIGNALS.FINISHED && rec.seen) continue;
      try {
        deliver({
          signal: entry.signal,
          paneId,
          agentType: rec.agentType,
          title: rec.title,
        });
      } catch { /* a failing sink must not stop the others */ }
    }
  }

  return {
    onStateEvent,
    flush,
    forget(paneId) { pendingByPane.delete(paneId); },
    pending(paneId) { return pendingByPane.get(paneId) ?? null; },
  };
}

module.exports = { createNotifier, SIGNALS };
