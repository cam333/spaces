'use strict';

/**
 * Run collaboration table migrations on the given better-sqlite3 database.
 * Safe to call multiple times — uses IF NOT EXISTS.
 */
exports.runMigrations = function runMigrations(db) {
  db.exec(`
    CREATE TABLE IF NOT EXISTS workspace_messages (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      workspace_id INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
      pane_id TEXT REFERENCES panes(id) ON DELETE SET NULL,
      sender_name TEXT NOT NULL DEFAULT 'System',
      type TEXT NOT NULL DEFAULT 'message',
      content TEXT NOT NULL,
      target_pane_id TEXT REFERENCES panes(id) ON DELETE SET NULL,
      status TEXT NOT NULL DEFAULT 'delivered',
      created DATETIME DEFAULT CURRENT_TIMESTAMP,
      metadata TEXT DEFAULT '{}'
    );

    CREATE TABLE IF NOT EXISTS workspace_context (
      workspace_id INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
      key TEXT NOT NULL,
      value TEXT NOT NULL DEFAULT '',
      updated_by TEXT NOT NULL DEFAULT 'System',
      updated DATETIME DEFAULT CURRENT_TIMESTAMP,
      PRIMARY KEY (workspace_id, key)
    );
  `);
};
