// bin/lib/runtime/db-watch.js
'use strict';

/**
 * Notices writes to spaces.db made by anything other than this runtime — the
 * web app's API routes, the network API, another process — and replays them
 * onto the event hub as the events the runtime emits for its own writes.
 *
 * The web app runs in a separate process and writes the database directly,
 * so before this a space renamed in the browser reached the terminal client
 * only on its next reconnect. Watching the shared database instead of
 * routing every web mutation through the runtime catches every writer
 * without touching any of them.
 *
 * `PRAGMA data_version` is the change signal: SQLite moves it only when a
 * *different* connection commits, and the watcher reads it through the
 * session store's own connection, so the runtime's own writes never register.
 * Those are tracked by listening to the hub instead, and simply re-baseline.
 *
 * When an outside write and a runtime write land in the same tick the diff
 * covers both, so the runtime's own change is emitted a second time. Every
 * event here is idempotent for a client folding it, and a pane the runtime
 * already holds is never re-announced, so the duplicate is harmless.
 */

const POLL_MS = 500;

/** Events the runtime emits for its own structural writes. */
const STRUCTURAL = new Set([
  'workspace.created', 'workspace.updated', 'workspace.removed',
  'pane.created', 'pane.removed', 'pane.moved', 'pane.updated',
]);

/** The rows the snapshot is built from, plus the connection's data_version. */
function readRows(d) {
  const version = d.pragma('data_version', { simple: true });
  const workspaces = new Map();
  for (const w of d.prepare('SELECT id, name, color FROM workspaces').all()) {
    workspaces.set(`w${w.id}`, { name: w.name ?? '', color: w.color ?? null });
  }
  // Same tolerance as snapshot(): an older or partial schema may lack these.
  const columns = new Set(d.prepare('PRAGMA table_info(panes)').all().map(c => c.name));
  const optional = ['title', 'color'].filter(c => columns.has(c));
  const select = ['id', 'workspace_id', 'agent_type', 'cwd', ...optional].join(', ');
  const panes = new Map();
  for (const p of d.prepare(`SELECT ${select} FROM panes`).all()) {
    panes.set(String(p.id), {
      workspaceId: p.workspace_id != null ? `w${p.workspace_id}` : null,
      agentType: p.agent_type || 'shell',
      cwd: p.cwd || '',
      title: p.title ?? '',
      // A cleared pane color is stored as '' (the column is NOT NULL).
      color: p.color || null,
    });
  }
  return { version, workspaces, panes };
}

/**
 * The events that turn `before` into `after`, as `[type, payload]` pairs in
 * an order a client can fold: spaces appear before the panes inside them,
 * and panes leave before the space that held them.
 */
function diffRows(before, after) {
  const events = [];

  for (const [id, w] of after.workspaces) {
    const old = before.workspaces.get(id);
    if (!old) {
      events.push(['workspace.created', { workspaceId: id, name: w.name }]);
      // workspace.created carries no color on the wire.
      if (w.color) events.push(['workspace.updated', { workspaceId: id, color: w.color }]);
      continue;
    }
    const fields = {};
    if (old.name !== w.name) fields.name = w.name;
    if (old.color !== w.color) fields.color = w.color;
    if (Object.keys(fields).length) events.push(['workspace.updated', { workspaceId: id, ...fields }]);
  }

  const removedSpaces = new Set([...before.workspaces.keys()].filter(id => !after.workspaces.has(id)));

  for (const [id, p] of after.panes) {
    const old = before.panes.get(id);
    if (!old) {
      events.push(['pane.created', {
        paneId: id, workspaceId: p.workspaceId, agentType: p.agentType, cwd: p.cwd, live: false,
      }]);
      events.push(['pane.updated', { paneId: id, title: p.title, color: p.color }]);
      continue;
    }
    if (old.workspaceId !== p.workspaceId && p.workspaceId) {
      events.push(['pane.moved', { paneId: id, workspaceId: p.workspaceId }]);
    }
    const fields = {};
    if (old.title !== p.title) fields.title = p.title;
    if (old.color !== p.color) fields.color = p.color;
    if (Object.keys(fields).length) events.push(['pane.updated', { paneId: id, ...fields }]);
  }

  const closedBySpace = new Map([...removedSpaces].map(id => [id, []]));
  for (const [id, p] of before.panes) {
    if (after.panes.has(id)) continue;
    if (p.workspaceId && closedBySpace.has(p.workspaceId)) closedBySpace.get(p.workspaceId).push(id);
    else events.push(['pane.removed', { paneId: id }]);
  }
  for (const [workspaceId, closedPanes] of closedBySpace) {
    events.push(['workspace.removed', { workspaceId, closedPanes }]);
  }

  return events;
}

/**
 * Starts polling. `withConnection` is the session store's; `state` is the
 * runtime's pane state, kept in step so the next snapshot agrees with the
 * events. Returns `{ tick, stop }` — `tick` is exposed for tests.
 */
function createDbWatcher({ withConnection, hub, state, intervalMs = POLL_MS }) {
  const read = () => {
    const rows = withConnection(readRows);
    return rows && rows.workspaces ? rows : null;
  };

  let baseline = read();
  let ownWrite = false;
  let replaying = false;

  const unsubscribe = hub.subscribe((event) => {
    if (!replaying && STRUCTURAL.has(event.type)) ownWrite = true;
  });

  function replay(type, payload) {
    const rec = payload.paneId ? state.get(payload.paneId) : null;
    switch (type) {
      case 'pane.created':
        // The runtime already announced a pane it holds a record for.
        if (rec) return;
        break;
      case 'pane.updated':
        if (rec) {
          // Through the state store, so a rename from the web wins over the
          // shell's OSC title exactly as a rename from any client does. Each
          // setter emits its own pane.updated.
          if (payload.title !== undefined) state.setUserTitle(payload.paneId, payload.title);
          if (payload.color !== undefined) state.setColor(payload.paneId, payload.color);
          return;
        }
        break;
      case 'pane.moved':
        if (rec) rec.workspaceId = payload.workspaceId;
        break;
      case 'pane.removed':
        // A running pane outlives its row; its exit will report it.
        if (rec && rec.live) return;
        if (rec) state.remove(payload.paneId);
        break;
      case 'workspace.removed':
        for (const r of state.snapshotPanes()) {
          if (r.workspaceId === payload.workspaceId && !r.live) state.remove(r.paneId);
        }
        break;
    }
    hub.emit(type, payload);
  }

  function tick() {
    const now = read();
    if (!now) return;
    if (!baseline || now.version === baseline.version) {
      // Nothing from outside. Absorb the runtime's own writes, if any.
      if (!baseline || ownWrite) baseline = now;
      ownWrite = false;
      return;
    }
    const events = diffRows(baseline, now);
    baseline = now;
    ownWrite = false;
    replaying = true;
    try {
      for (const [type, payload] of events) replay(type, payload);
    } finally {
      replaying = false;
    }
  }

  const timer = setInterval(tick, intervalMs);
  if (timer.unref) timer.unref();

  return {
    tick,
    stop() {
      clearInterval(timer);
      unsubscribe();
    },
  };
}

module.exports = { createDbWatcher, diffRows, readRows, POLL_MS };
