'use strict';

const { evaluate } = require('./evaluate');
const { createDebouncer } = require('./debounce');

const TICK_MS = 300;

/**
 * Polls dirty panes' detection snapshots and commits debounced agent states
 * into the state store. Only panes that produced PTY output since the last
 * tick are evaluated — an idle pane costs nothing.
 * Timers and the clock are injected so tests drive it deterministically.
 */
function createScheduler({
  registry, getScreen, state,
  now = () => Date.now(),
  setIntervalFn = setInterval,
  clearIntervalFn = clearInterval,
  log,
} = {}) {
  const tracked = new Map(); // paneId -> { agentType }
  const dirty = new Set();
  const results = new Map();
  const debouncer = createDebouncer();
  let timer = null;

  function evaluatePane(paneId) {
    const entry = tracked.get(paneId);
    if (!entry) return;
    const screen = getScreen(paneId);
    if (!screen) return;

    let snapshot;
    try {
      snapshot = screen.detectionSnapshot();
    } catch (err) {
      // A disposed screen mid-tick is normal during pane teardown.
      if (log) log(`[detect] snapshot failed for ${paneId}: ${err.message}`);
      return;
    }

    const manifest = registry.get(entry.agentType);
    let result;
    try {
      result = evaluate(snapshot, manifest);
    } catch (err) {
      // Defence in depth: evaluate() is contracted to never throw, but this
      // runs inside a bare setInterval callback with no other guard — an
      // uncaught throw here would crash the process that also hosts the web
      // server. Record the failure rather than swallowing it silently, and
      // leave agentState untouched (same as a skipStateUpdate rule).
      if (log) log(`[detect] evaluate threw for ${paneId}: ${err.message}`);
      results.set(paneId, {
        state: 'unknown', ruleId: null, reason: `evaluate_threw: ${err.message}`, evidence: [],
        manifestSource: registry.sourceOf(entry.agentType),
        manifestVersion: manifest ? manifest.version : null,
        at: now(),
      });
      return;
    }
    results.set(paneId, {
      state: result.state,
      ruleId: result.ruleId,
      reason: result.reason,
      evidence: result.evidence,
      manifestSource: registry.sourceOf(entry.agentType),
      manifestVersion: manifest ? manifest.version : null,
      at: now(),
    });

    if (result.skipStateUpdate) return;

    const rec = state.get(paneId);
    if (!rec) return;
    const commit = debouncer.step(paneId, rec.agentState, result.state, now());
    if (commit) {
      state.setAgentState(paneId, commit);
    } else if (debouncer.pending(paneId)) {
      // A debounce hold is open (working/blocked -> idle, awaiting more
      // confirmations or the 700ms cap) but nothing produced PTY output
      // after this. markDirty is only ever called from feed(), so if the
      // agent's final render arrived in one burst and then went quiet, no
      // future tick would re-evaluate this pane and the hold would never
      // resolve — self re-dirty while a hold is open; step() clears the
      // hold within IDLE_CONFIRMATIONS ticks (<=3, <=700ms), so this can't
      // loop forever.
      dirty.add(paneId);
    }
  }

  function tick() {
    if (!dirty.size) return;
    const batch = [...dirty];
    dirty.clear();
    for (const paneId of batch) evaluatePane(paneId);
  }

  return {
    track(paneId, agentType) {
      // Re-tracking a paneId (e.g. registerPane re-registering a still-live
      // id) must not inherit the previous session's debounce hold or stale
      // evaluation result — clear both unconditionally so track() is
      // self-healing regardless of whether the caller paired the prior
      // session with an untrack() first.
      debouncer.forget(paneId);
      results.delete(paneId);
      tracked.set(paneId, { agentType });
      dirty.add(paneId);
    },
    untrack(paneId) {
      tracked.delete(paneId);
      dirty.delete(paneId);
      results.delete(paneId);
      debouncer.forget(paneId);
    },
    markDirty(paneId) { if (tracked.has(paneId)) dirty.add(paneId); },
    tick,
    lastResult(paneId) { return results.get(paneId) ?? null; },
    pendingIdle(paneId) { return debouncer.pending(paneId); },
    start() {
      if (timer) return;
      timer = setIntervalFn(tick, TICK_MS);
      if (timer && typeof timer.unref === 'function') timer.unref();
    },
    stop() {
      if (!timer) return;
      clearIntervalFn(timer);
      timer = null;
    },
  };
}

module.exports = { createScheduler, TICK_MS };
