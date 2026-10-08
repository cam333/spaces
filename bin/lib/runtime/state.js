'use strict';

const AGENT_STATES = ['working', 'blocked', 'idle', 'unknown'];

/** Rollup priority. 'done' is derived: idle && !seen. */
const PRIORITY = { blocked: 5, working: 4, done: 3, idle: 2, unknown: 1 };

class PaneStateStore {
  /** @param {import('./event-hub').EventHub} hub */
  constructor(hub) {
    this._hub = hub;
    this._panes = new Map();
  }

  open(paneId, { agentType = 'shell', cwd = '', workspaceId = null } = {}) {
    const existing = this._panes.get(paneId);
    if (existing && existing.live) return;
    const rec = {
      paneId, workspaceId, agentType,
      agentState: 'unknown', seen: true, focused: false,
      title: '', cwd, sessionRef: null, live: true, lastStateChange: 0,
      // A color the user chose (pane.update), or null to fall back to the DB
      // row / no accent at all. Distinct from `title` in that nothing else
      // ever writes it — there is no color equivalent of an OSC sequence.
      color: null,
      // Set once the user names the pane themselves. The PTY emits an OSC
      // title sequence constantly (every `cd`, every command a shell runs),
      // so without this a user-chosen name is overwritten within
      // milliseconds — which is why `pane.update` needed more than a setter.
      userTitle: false,
    };
    this._panes.set(paneId, rec);
    this._hub.emit('pane.opened', { paneId, workspaceId, agentType, cwd });
  }

  get(paneId) { return this._panes.get(paneId); }

  close(paneId, exitCode) {
    const rec = this._panes.get(paneId);
    if (!rec || !rec.live) return;
    rec.live = false;
    this._hub.emit('pane.closed', { paneId, exitCode });
  }

  // Drops a pane's record entirely, unlike close() which keeps a live:false
  // record around so a naturally-exited pane still shows as "recently
  // exited". Used by protocol-initiated close (pane.close/workspace.close in
  // index.js) where there is nothing worth showing afterward — the record
  // (and its workspaceId) must stop being visible to snapshot() at all, or a
  // closed workspace gets resurrected as a nameless stub around it. No event
  // is emitted: the caller's own pane.removed/workspace.removed already
  // covers the client-facing signal for this.
  remove(paneId) {
    this._panes.delete(paneId);
  }

  setAgentState(paneId, next) {
    const rec = this._panes.get(paneId);
    if (!rec || rec.agentState === next) return;
    if (!AGENT_STATES.includes(next)) return;
    const prev = rec.agentState;
    rec.agentState = next;
    const completion = (prev === 'working' || prev === 'blocked') && next === 'idle';
    if (next !== 'idle') rec.seen = true;
    else if (completion) rec.seen = rec.focused;
    rec.lastStateChange = this._hub.emit('pane.state', {
      paneId, agentState: next, seen: rec.seen,
    });
  }

  markSeen(paneId) {
    const rec = this._panes.get(paneId);
    if (!rec || rec.seen) return;
    rec.seen = true;
    this._hub.emit('pane.seen', { paneId });
  }

  setFocused(paneId, focused) {
    const rec = this._panes.get(paneId);
    if (!rec) return;
    rec.focused = focused;
    if (focused) this.markSeen(paneId);
  }

  // Focus is exclusive: exactly one pane may be focused at a time. Without
  // this, `pane.focus` was one-way (nothing ever called setFocused(id,
  // false)), so once any pane was focused, every previously-focused pane
  // stayed `focused: true` forever — permanently pinning `seen = rec.focused`
  // to true on its next completion, so it could never again render as "done".
  setExclusiveFocus(paneId) {
    if (!this._panes.has(paneId)) return;
    for (const [id, rec] of this._panes) {
      if (id !== paneId && rec.focused) this.setFocused(id, false);
    }
    this.setFocused(paneId, true);
  }

  /**
   * The PTY's own OSC title. Ignored once the user has named the pane
   * themselves — see `userTitle` on the record. A shell emits one of these on
   * every `cd` and every command it runs, so without the guard a user-chosen
   * name survives for milliseconds.
   */
  setTitle(paneId, title) {
    const rec = this._panes.get(paneId);
    if (!rec || rec.userTitle || rec.title === title) return;
    rec.title = title;
    this._hub.emit('pane.title', { paneId, title });
  }

  /**
   * A title the *user* chose. Wins over every subsequent OSC title, and
   * emits `pane.updated` rather than `pane.title` so a client can tell "the
   * shell renamed itself" from "someone renamed this".
   *
   * Passing an empty string hands the pane back to its OSC title, which is
   * the only way to undo a rename.
   */
  setUserTitle(paneId, title) {
    const rec = this._panes.get(paneId);
    if (!rec) return false;
    rec.userTitle = title !== '';
    rec.title = title;
    this._hub.emit('pane.updated', { paneId, title });
    return true;
  }

  /** A color the user chose. Nothing else ever writes this. */
  setColor(paneId, color) {
    const rec = this._panes.get(paneId);
    if (!rec) return false;
    rec.color = color;
    this._hub.emit('pane.updated', { paneId, color });
    return true;
  }

  setSessionRef(paneId, ref) {
    const rec = this._panes.get(paneId);
    if (!rec) return;
    const newSessionRef = { id: ref.id, transcriptPath: ref.transcriptPath ?? null };
    if (rec.sessionRef && rec.sessionRef.id === newSessionRef.id && rec.sessionRef.transcriptPath === newSessionRef.transcriptPath) return;
    rec.sessionRef = newSessionRef;
    this._hub.emit('pane.session', { paneId, sessionRef: rec.sessionRef });
  }

  snapshotPanes() {
    return [...this._panes.values()].map(r => ({ ...r }));
  }

  static rollup(records) {
    let best = 0;
    for (const r of records) {
      const key = r.agentState === 'idle' && !r.seen ? 'done' : r.agentState;
      best = Math.max(best, PRIORITY[key] || 0);
    }
    const names = Object.entries(PRIORITY);
    for (const [name, p] of names) if (p === best) return name;
    return 'unknown';
  }
}

module.exports = { PaneStateStore, AGENT_STATES };
