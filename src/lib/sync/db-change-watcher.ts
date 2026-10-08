import { getDb } from '../db/schema';
import { sseManager } from '../events/sse';

/**
 * Tells browsers when another process — the terminal client, through the
 * runtime — has changed the spaces or panes in the database.
 *
 * The runtime runs in the parent process and writes spaces.db on its own
 * connection, so nothing in this process hears about it. `PRAGMA
 * data_version` on this process's connection moves only when a *different*
 * connection commits, which makes it a free "did someone else write?" check.
 * Plenty of other writers touch other tables (agents posting messages, for
 * one), so a write only counts once the spaces and panes it could have
 * changed actually differ.
 */

const POLL_MS = 750;

let started = false;

function fingerprint(): string {
  const db = getDb();
  const spaces = db.prepare('SELECT id, name, color FROM workspaces ORDER BY id').all();
  const panes = db
    .prepare('SELECT id, title, color, workspace_id FROM panes ORDER BY id')
    .all();
  return JSON.stringify([spaces, panes]);
}

export function initDbChangeWatcher() {
  if (started) return;
  started = true;
  // Multiuser mode has no runtime writing this database (it refuses /proto),
  // and getDb() is per-user there, which a timer has no user to resolve for.
  if (process.env.SPACES_MULTIUSER === '1') return;

  let version: number;
  let last: string;
  try {
    version = getDb().pragma('data_version', { simple: true }) as number;
    last = fingerprint();
  } catch {
    return;
  }

  const timer = setInterval(() => {
    try {
      const now = getDb().pragma('data_version', { simple: true }) as number;
      if (now === version) return;
      version = now;
      const current = fingerprint();
      if (current === last) return;
      last = current;
      sseManager.broadcast('spaces:changed', { timestamp: Date.now() });
    } catch {
      // A transiently locked or missing database is retried on the next tick.
    }
  }, POLL_MS);
  timer.unref?.();
}
