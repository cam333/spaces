# Claude Plan-Usage Display

**Status:** stable
**Owner:** Spaces core
**Last updated:** 2026-04-22

## Overview

A compact readout of the user's Anthropic plan-usage rate limits, surfaced in the terminal page header whenever the active workspace has at least one Claude pane. Shows 5-hour, 7-day Opus, and 7-day Sonnet windows at a glance; click expands to all windows plus overage credits. Data matches what `/usage` shows inside Claude Code.

## User-facing behavior

- Open any workspace on `/terminal` that has a Claude pane.
- A horizontal strip appears in the header (right side, before Files / Collab / Close / Add Pane).
- Three thin progress bars: **5h**, **Opus**, **Sonnet** — each with a percent label and a color that escalates green → amber → red.
- Hover a bar → tooltip `{label}: X% used · resets in 2h 14m`.
- Click the strip → popover with all five rate-limit windows + an overage-credits block if the account has overage enabled.
- If the OAuth token is missing or expired, the strip shows `sign in via Claude Code` with a small login icon. No auto-refresh.
- If the Anthropic endpoint fails or times out, the strip hides silently.

## Architecture

UI (`src/components/claude/claude-usage-strip.tsx`) → polling hook (`src/hooks/use-claude-usage.ts`) → API route (`src/app/api/claude/usage/route.ts`) → credentials reader (`src/lib/claude/credentials.ts`) + Anthropic fetcher with 30s cache (`src/lib/claude/usage.ts`) → `https://api.anthropic.com/api/oauth/usage`.

Polling: hook calls the route on mount and every 60s while the strip is enabled. Server caches the Anthropic response 30s per access-token prefix so rapid re-polls don't hammer Anthropic.

## Key files

| Path | Role |
|---|---|
| `src/lib/claude/credentials.ts` | Reads `~/.claude/.credentials.json`, extracts `claudeAiOauth`, exposes `isTokenExpired()`. |
| `src/lib/claude/usage.ts` | Calls Anthropic's `/api/oauth/usage`, 5s timeout, 30s in-memory cache. |
| `src/app/api/claude/usage/route.ts` | `GET` handler; returns one of `ok` / `unauthenticated` / `expired` / `upstream_error`. |
| `src/hooks/use-claude-usage.ts` | 60s polling hook, toggled by `enabled` arg. |
| `src/components/claude/claude-usage-strip.tsx` | Compact 3-bar strip + expanded popover. |
| `src/app/(desktop)/terminal/page.tsx` | Mounts the strip conditionally in the header. |
| `src/types/claude.ts` | `RateLimit`, `ExtraUsage`, `Utilization`, `UsageResponse`. |

## Data model

No DB tables. All state is transient:
- Server-side: in-memory `Map<tokenPrefix, { fetchedAt, data }>` in `src/lib/claude/usage.ts` with 30s TTL. Process-local; resets on server restart.
- Client-side: React state in the hook.

## APIs / contracts

### Internal endpoints

- `GET /api/claude/usage` — returns `UsageResponse`:
  - `200 { status: "ok", data: Utilization }` — happy path.
  - `200 { status: "unauthenticated" }` — no credentials file or no `claudeAiOauth`.
  - `200 { status: "expired" }` — access token past `expiresAt`.
  - `200 { status: "upstream_error" }` — Anthropic non-2xx or timeout.

Auth: same `withUser`/`getAuthUser` wrapping as every other `/api/*` route.

### External calls

- **Anthropic `GET https://api.anthropic.com/api/oauth/usage`** — uses the user's own OAuth access token read from `~/.claude/.credentials.json`. No secret of our own. User-Agent `claude-code/spaces-integration`.

## Configuration & environment

- `CLAUDE_CONFIG_DIR` — respected (defaults to `~/.claude`). Same semantics as Claude Code.
- No env vars, feature flags, or tier gates specific to this feature.

## How to run / test locally

```bash
npm install
npm run build
npm start
# visit http://localhost:3457/terminal, enter a workspace, add a Claude pane
```

Manual QA checklist:

1. With a valid OAuth session (logged in via any recent Claude Code run): the strip appears in the header; percents match `/usage` inside Claude Code.
2. Hover each bar → tooltip shows the reset timing.
3. Click → popover with all five windows and overage block (if applicable).
4. Delete the Claude pane from the workspace → strip disappears.
5. Set `claudeAiOauth.expiresAt` to a past timestamp in `.credentials.json`, reload → strip shows "sign in via Claude Code".
6. Move `.credentials.json` aside temporarily, reload → same sign-in hint.
7. Disconnect network → within ~90s the strip silently hides (`upstream_error` → null render).

No automated tests for v1.

## Known quirks & decisions

- **Passive refresh.** We do not refresh the OAuth access token. Claude Code owns the refresh lifecycle and writes `.credentials.json`; racing with it would corrupt the shared file.
- **Account-wide data, not per-pane.** The Anthropic endpoint returns account-level rate limits. Rendering once in the header (not per-pane) is intentional.
- **User-Agent and anthropic-beta header.** The Anthropic `/api/oauth/usage` endpoint requires the `anthropic-beta: oauth-2025-04-20` header in addition to the Bearer token — without it the call 4xx-errors out. Both the UA (`claude-code/spaces`) and the beta header are constants in `src/lib/claude/usage.ts`; if upstream ever rotates the beta header, bump it there.
- **30s cache + 60s poll.** Outer-bound staleness is ~90s; fine for windows that move in hours.
- **Token-prefix cache key.** First 16 chars of `accessToken`. Collision probability is effectively nil; avoids holding full tokens in the cache structure beyond the single request.
- **Silent upstream failure.** Errors hide the strip rather than showing a red toast — rate-limit info is nice-to-have, not mission-critical.
- **Ships direct to `main`.** Per repo convention.

## Open questions / TODOs

- [ ] Active OAuth refresh — mirror Claude Code's `/oauth/token` dance to keep the widget live past expiry. Deferred for v1.
- [ ] Session-cost complement using local JSONL token counts (different data source).
- [ ] Consider surfacing 7-day-overall alongside the current three for users who care.

## Changelog

- 2026-04-22 — Initial implementation. v1 ships direct to `main`.
