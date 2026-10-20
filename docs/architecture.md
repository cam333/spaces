# Architecture

A developer's map of the Spaces codebase: what runs where, how the pieces
talk, and where authentication happens.

## The pieces

```
  terminal client (Rust)          browser (React, xterm.js)
  tui/crates/spaces-tui           src/app, src/components
        │  /proto, /proto/pane/<id>     │  HTTP, /ws
        ▼                               ▼
  ┌───────────────────────────────────────────────────────────┐
  │ launcher + front server       bin/spaces.js               │
  │ 127.0.0.1:3457 (HTTP and HTTPS on one port)               │
  │   ├─ WebSocket upgrades ─► terminal server                │
  │   │                        bin/terminal-server.js         │
  │   │                        └─ runtime  bin/lib/runtime/   │
  │   │                           panes (node-pty), detection,│
  │   │                           events, files, git status   │
  │   └─ everything else ────► Next.js app, 127.0.0.1:3400    │
  │                            src/app (pages + /api routes)  │
  │                            src/middleware.ts (auth)       │
  └───────────────────────────────────────────────────────────┘
        │                               │
        ▼                               ▼
  ~/.spaces/spaces.db (SQLite)   ~/.claude, ~/.codex, ~/.gemini (read-only)
```

There is one Node process tree per machine: the launcher process holds the
public port and the terminal server, and runs Next.js as a child process.

## Launcher: `bin/spaces.js`

The `spaces` command. It:

- parses the command line and dispatches subcommands (`tui`, `start`,
  `open`, `signout`, `stop`, `service`, `upgrade`, `guide`, ...);
- for the terminal client, finds the platform binary (`bin/spaces-tui.js`;
  an npm optional dependency `agent-spaces-tui-<platform>-<arch>`, or a
  `cargo` build in a checkout), starts a detached server if nothing is
  listening, signs a session token and runs the binary with it in the
  environment;
- for `--remote`, does the same through SSH (`bin/lib/remote.js`):
  `ssh <dest> spaces start --print-token` on the other machine, then an SSH
  tunnel from a free local port to the remote's loopback port;
- otherwise runs the server: creates or reads the owner's secret, starts
  Next.js on the internal port (`PORT - 57`), and serves the public port
  itself. The first byte of each connection decides HTTP or TLS, so one port
  serves both. Plain requests are proxied to Next.js; WebSocket upgrades go
  to the terminal server.

The front server also enforces the Host check and redeems one-time sign-in
links (`/auth/desktop?code=...`), because the single-use bookkeeping lives
in this process.

It also runs the update check (npm registry and GitHub releases, every
4 hours) and, only when asked, the experimental federation listener
(`bin/lib/network/`).

## Terminal server and runtime

`bin/terminal-server.js` attaches to the front server's HTTP servers and
handles three WebSocket endpoints:

| Endpoint | Used by | What |
| --- | --- | --- |
| `/ws` | the web app's terminal panes | The original per-pane terminal socket: spawn or reattach a PTY, stream bytes. |
| `/proto` | the terminal client | The control channel: snapshot, method calls, live events. |
| `/proto/pane/<id>` | the terminal client | A pane's data channel: PTY bytes down, input and resize up. |

The runtime (`bin/lib/runtime/`) is the state behind both. It owns:

- **Panes** (`spawn-pane.js`): one `node-pty` process per pane, with an
  allowlisted environment (the server's own secrets are never passed in)
  and the agent table that knows how to launch and resume each agent.
  A headless screen model per pane (`screen.js`) lets a late client replay
  the current screen. One client at a time controls a pane; others observe.
- **Agent state** (`detect/`): declarative per-agent rules (`manifests/`)
  read the pane's screen and classify it as working, blocked, idle or done.
  The bundled rules can be overridden in `~/.spaces/detection/` and are
  refreshed daily from `agentspaces.co` unless `SPACES_NO_DETECTION_UPDATE=1`.
  Rules are data, validated and size-capped, never code.
- **Events** (`event-hub.js`, `notify.js`): pane, workspace and agent-state
  events fan out to every `/proto` client. `db-watch.js` turns writes the
  web app makes to `spaces.db` into events, which is how the two clients
  stay in step.
- **Files** (`files.js`): `fs.*` methods for the terminal client's file
  browser, with the confinement rules described under
  [Authentication](#authentication-and-confinement).
- **Git status** (`git-status.js`) and **Claude session tracking**
  (`claude-sessions.js`), so a restarted pane resumes its conversation.

The protocol is documented in [docs/protocol](protocol/README.md). Its JSON
Schema (`docs/protocol/spaces-proto.schema.json`) is generated from the
runtime's Zod definitions (`npm run proto:schema`); CI fails if the schema
drifts (`npm run proto:check`) or a method goes undocumented
(`npm run proto:docs`). The Rust client generates its wire types from the
same schema at build time.

Running `node bin/terminal-server.js` on its own starts a standalone
terminal server on `SPACES_WS_PORT` (default 3458). Nothing in a normal
install does this; it exists for development and tests.

## Next.js app

`src/` is a Next.js App Router application.

- `src/app/(desktop)/`: the desktop web UI: terminal grid, sessions,
  dashboard and analytics, projects, settings. `src/app/m/` is the mobile
  layout, `src/app/vr/` the VR room.
- `src/app/api/`: the HTTP API: workspaces, panes, messages and context
  (the bus `spaces-msg` uses), todos, sessions and search, files, config,
  updates, and the optional wizard and voice routes.
- `src/lib/db/`: the SQLite schema and queries for `~/.spaces/spaces.db`,
  including the FTS5 full-text index of agent sessions.
- `src/lib/sync/`, `src/lib/claude/`, `src/lib/codex/`, `src/lib/gemini/`,
  `src/lib/aider/`: read each agent's history from disk and index it.
- `src/lib/terminal/collaboration.js`: when collaboration is on for a
  space, writes the `spaces-bus` MCP server into the pane folder's
  `.mcp.json` (Claude Code) or `.codex/config.toml` (Codex).

### Helper commands

- `bin/spaces-msg.js` (`spaces-msg`): the message-bus CLI for use inside a
  pane. It reads `SPACES_WORKSPACE_ID`, `SPACES_PANE_ID` and
  `SPACES_API_URL`, which every pane's environment carries.
- `bin/spaces-mcp.js`: the same bus as an MCP server over stdio.
- `bin/spaces-hook.js`: the Claude Code hook that delivers bus messages to
  a collaborating pane.
- `bin/spaces-service.js`: `spaces service ...`.

Each of these runs as the owner and authenticates with a 60-second token it
signs itself with the owner's secret.

## Terminal client

`tui/` is a Cargo workspace with two crates:

- `spaces-proto`: wire types and an async client for `/proto` and
  `/proto/pane/<id>`. It knows nothing about the runtime's internals.
- `spaces-tui`: the full-screen client, built on ratatui and crossterm.
  `model/` holds client state, `input.rs` routes keys (palette, chords,
  pane input), `panes.rs` keeps pane data sockets open and sized, `term.rs` is the
  per-pane terminal emulator,
  `files.rs`, `editor.rs` and `transfer.rs` are the Files tab, and `ui/`
  draws the sidebar, tabs, palette, menus and toasts.

The client is a peer like any other: `spaces-tui --url ws://host:port`, with
the session token in `SPACES_TOKEN` (or `--token`). The launcher fills both
in.

## Authentication and confinement

Desktop mode (`bin/lib/desktop-auth.js`, `src/lib/auth/desktop.ts`,
`src/middleware.ts`) has one rule: every request proves it comes from the
owner.

- **The secret** is `~/.spaces/internal_token`: 32 random bytes, mode 0600
  inside a 0700 directory (on Windows, inside the user's profile). The
  server tightens loose permissions at start-up and refuses to start if the
  file belongs to another account. `SPACES_INTERNAL_TOKEN` overrides it.
- **Tokens** are HMAC-signed with the secret and carry a kind and an
  expiry: `session` (browser cookie and terminal client, 7 days), `api`
  (60 seconds, minted per request by the helper commands) and `login`
  (single use, 2 minutes, from `spaces web`). The secret itself never goes
  on the wire, and credentials are read from headers and cookies only,
  never from URLs.
- **The browser** holds the session in the `spaces-desktop` cookie
  (HttpOnly, SameSite=Strict). The cookie counts only on requests that come
  from Spaces' own pages, since a page on another localhost port is the
  same site to the browser.
- **The Host header** must name the loopback (or a name in
  `SPACES_ALLOWED_ORIGINS`), checked in both the front server and the
  middleware, which defeats DNS rebinding. The internal Next.js port runs
  the same middleware, so reaching it directly gains nothing.
- **Before handing out a credential**, the launcher checks (on Linux) that
  the process listening on the port belongs to the same account, so another
  user cannot squat the port while Spaces is down and collect a token.
  On Linux, `spaces web` passes the sign-in link through an owner-only file
  rather than the browser's command line, which other accounts can read.
- **`spaces signout`** writes a new secret and restarts the server, which
  invalidates every token at once.

**File access** from both clients is confined to the configured dev
directories (`devDirectories` in `~/.spaces/config.json`, defaulting to the
home folder) plus the agents' transcript folders, checked after resolving
symlinks. Credential locations (`~/.spaces`, `~/.ssh`, `~/.gnupg`, `~/.aws`,
`~/.azure`, `~/.kube`, `~/.docker`, `~/.config/gcloud`, `~/.config/gh`,
`~/.netrc`, `~/.npmrc`, `~/.pypirc`, `~/.git-credentials`) and the rest of
the agents' config folders are refused even inside a dev directory. The web
app's rules (`src/app/api/files/route.ts`) and the runtime's
(`bin/lib/runtime/files.js`) are kept in step by tests.

### Experimental modes

Multiuser mode (`src/lib/auth/`, `bin/lib/auto-setup.js`) and federation
(`bin/lib/network/`) are outside the supported security model; see
[SECURITY.md](../SECURITY.md). In multiuser mode both `/proto` endpoints are
refused, because panes have no per-user ownership yet.

## Tests

- `tests/` (vitest): the runtime and protocol (`proto/`, `runtime/`),
  agent detection (`detect/`), the launcher's helpers (`lib/`, `tui/`),
  security regressions (`security/`), the database (`db/`) and the
  experimental network code (`network/`). `npm test` runs them all.
- `tui/` (cargo): unit tests in each module, plus an integration test,
  `cargo test -p spaces-tui --test live_runtime -- --ignored`, that boots
  the real Node runtime (it needs `npm ci` first).
