# Changelog

All notable changes to Spaces are recorded here. The project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

- On Windows, a pane's agent conversation was never recorded, so panes
  restarted after a server restart or reboot began a new conversation
  instead of resuming the old one. The pane is now found through its
  console, which survives the way the agent's launcher re-executes itself.

## 1.0.1

- `spaces open --remote you@host`: the web app for Spaces on another machine,
  through an SSH tunnel, with the browser signed in and kept apart from
  this machine's own sign-in.
- `spaces --remote` offers to set up SSH key sign-in when the other machine
  asks for a password, which it would otherwise do for each of its three
  connections.
- `spaces --remote` installs into `~/.local` when npm's global folder needs
  root, and opens fewer SSH sessions.
- `spaces --remote` on Windows: a password typed for the tunnel is no longer
  garbled, and each SSH step says what it is doing.

## 1.0.0

The first public open-source release, under the Apache License 2.0.

### Workspaces and agents

- Named, coloured, persistent spaces of terminal panes. Panes run on the
  server and keep running when no client is attached; panes stopped by a
  restart or reboot start again when you open their space, and Claude Code
  panes resume their conversation.
- Agents per pane: Claude Code, Codex CLI, Gemini CLI, Aider, ForgeCode, Pi,
  a plain shell, or any command. Starting an agent offers to resume one of
  the folder's past sessions where the agent supports it.
- Agent-state detection (working, blocked, idle, done) for Claude Code,
  Codex CLI, Gemini CLI and Aider, with notifications when an agent blocks
  or finishes. Detection rules are declarative data, can be overridden
  locally, and update daily (`SPACES_NO_DETECTION_UPDATE=1` turns that off).
- A message bus between the panes of a space: `spaces-msg` inside any
  pane, and an MCP server (`spaces-bus`) that Spaces registers for Claude
  Code and Codex when collaboration is on.
- Git branch, ahead/behind and dirty state for each space.

### Terminal client

- A full-screen terminal client, prebuilt for Linux (x64, arm64), macOS
  (x64, arm64) and Windows (x64). Bare `spaces` opens it, starting the
  server in the background first if needed.
- Sidebar of spaces and agents, a tab per pane, a split view, a menu on
  `ctrl+k`, and `ctrl+space` chord keys for detach, split, files, sidebar,
  sort and mouse release. Mouse support throughout, including resizing the
  sidebar and a right-click menu on tabs.
- A Files tab per space: browse with live, syntax-highlighted previews;
  download and upload files or whole folders (including by dragging files
  onto the tab); open images and PDFs locally; create, rename, move and
  delete; and edit text files with conflict detection.
- `spaces --remote you@host`: the terminal client for Spaces on another
  machine, over SSH, starting Spaces there if needed. Nothing is exposed
  beyond your SSH login. It offers to install Spaces there, or to update an
  older install, saying first how many running agents an update would stop;
  `--yes` skips the questions.
- `spaces status` and `spaces restart` (through the service when one is
  installed).
- Live sync with the web app: changes on either side appear on the other
  within about a second.

### Web app

- A browser terminal grid for the same spaces and panes (`spaces open`).
- A session browser with local full-text search across Claude Code, Codex,
  Gemini CLI and Aider history.
- A dashboard with session counts, token usage by model, an activity heatmap
  and cost estimates.
- Per-space todos, a file viewer, a project wizard (with
  `ANTHROPIC_API_KEY`), voice input (with `GROQ_API_KEY` or
  `OPENAI_API_KEY`), a mobile layout and an experimental VR room.

### Security

- Desktop mode answers only its owner: loopback-only, every request
  authenticated with tokens derived from an owner-only secret, Host checks
  against DNS rebinding, one-time browser sign-in links (`spaces open`),
  no credentials in URLs, and `spaces signout` to revoke every session.
- File browsing confined to your dev directories and agent transcripts,
  with credential folders always refused.

### Operations

- `spaces start`, `stop`, `open`, `signout`, `upgrade`, `uninstall` and
  `guide`; `spaces service` to run Spaces at login on Linux, macOS and
  Windows.
- A documented, versioned session-control protocol (`/proto`) with a
  generated JSON Schema, which the terminal client is built on.

### Experimental (off by default, not supported)

- Multiuser mode and machine-to-machine federation, behind
  `--experimental`. Neither is safe for untrusted users or networks; see
  [SECURITY.md](SECURITY.md).
