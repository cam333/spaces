#!/usr/bin/env node
// Test-only bootstrap for `tests/live_runtime.rs`.
//
// `bin/spaces.js` refuses to start without a built `.next/standalone` or
// `.next/BUILD_ID` (see its "No build found" check) — this worktree has
// neither, and building one just to prove the Rust client agrees with the real
// runtime would be slow and unrelated to what this test verifies. The obvious
// alternative is to boot `bin/terminal-server.js` directly instead, but that
// file's own `require.main === module` block only wires up the legacy `/ws` PTY
// endpoint (`setupWss`) — it never calls `createTerminalServer`, which is the
// function that actually mounts `/proto` and `/proto/pane/<id>` (see the
// upgrade handler in `bin/terminal-server.js`). So this script calls
// `createTerminalServer` itself, the same way `bin/spaces.js` and
// `bin/spaces-dev.js` do, mounted on a bare `http.Server` with no Next.js in
// front of it — `/proto` doesn't need the web app, only the runtime-core
// WebSocket layer.
//
// `session.snapshot`'s `panes`/`workspaces` arrays are read straight out of
// `<home>/.spaces/spaces.db` (see `snapshot()` in `bin/lib/runtime/index.js`),
// and `sessionStore.createWorkspace`/`closeWorkspace` (session-store.js)
// require that file to already exist with a `workspaces`/`panes` schema —
// unlike the real app, this runtime never creates or migrates that schema
// itself (that's `src/lib/db/schema.ts`, which only the Next.js API layer
// calls). So this script creates the same minimal schema
// `tests/proto/workspace-control.test.ts` uses before starting the server.
//
// The caller (tests/live_runtime.rs) is responsible for pointing HOME/
// USERPROFILE at a fresh per-test directory before spawning this script —
// `os.homedir()` is what both the schema path below and the runtime's own
// `spacesDbPath` resolve against, so that's what keeps this test from ever
// touching a developer's real `~/.spaces/spaces.db`.

'use strict';

const fs = require('fs');
const path = require('path');
const os = require('os');
const http = require('http');

const terminalServerPath = process.argv[2];
if (!terminalServerPath) {
  console.error('boot_terminal_server.js: usage: node boot_terminal_server.js <path-to-bin/terminal-server.js>');
  process.exit(1);
}

const port = parseInt(process.env.SPACES_PORT, 10);
if (!Number.isFinite(port) || port <= 0) {
  console.error('boot_terminal_server.js: SPACES_PORT must be set to a valid port');
  process.exit(1);
}

const spacesDir = path.join(os.homedir(), '.spaces');
fs.mkdirSync(spacesDir, { recursive: true });
const dbPath = path.join(spacesDir, 'spaces.db');

const Database = require('better-sqlite3');
const db = new Database(dbPath);
db.exec(`
  CREATE TABLE IF NOT EXISTS workspaces (
    id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT
  );
  CREATE TABLE IF NOT EXISTS panes (
    id TEXT PRIMARY KEY, title TEXT, color TEXT, cwd TEXT,
    agent_type TEXT DEFAULT 'shell', sort_order INTEGER DEFAULT 0,
    workspace_id INTEGER REFERENCES workspaces(id) ON DELETE CASCADE
  );
`);
db.close();

// eslint-disable-next-line import/no-dynamic-require
const { createTerminalServer } = require(terminalServerPath);

const server = http.createServer((req, res) => {
  res.writeHead(404, { 'Content-Type': 'text/plain' });
  res.end('not found');
});

createTerminalServer(server);

server.listen(port, '127.0.0.1', () => {
  console.log(`boot_terminal_server.js: listening on ws://127.0.0.1:${port}/proto`);
});
