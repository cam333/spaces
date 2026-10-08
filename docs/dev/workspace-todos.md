# Workspace Todos

**Status:** stable
**Owner:** Spaces core
**Last updated:** 2026-04-21

## Overview

A lightweight todo list attached to each workspace. Renders inline at the top of `/terminal` above the pane grid, always visible. Minimal model: text + checkbox + delete. Scoped per-workspace — switching workspaces swaps the list. Not in scope: agent read/write, drag-reorder, priorities/due dates, cross-workspace views.

## User-facing behavior

- Visit `/terminal`, enter any workspace.
- A "Todos" bar renders above the pane grid with an "Add todo…" input.
- Enter adds a todo at the bottom of the uncompleted list. Input stays focused.
- Click the checkbox → strike-through + sink to the completed section.
- Click text → inline edit. Enter saves, Esc cancels, blur saves.
- Hover row → `×` delete button.
- Eye icon in the header toggles visibility of completed todos (ephemeral, resets on reload).

## Architecture

UI (`src/components/workspace/workspace-todos.tsx`) → API routes (`src/app/api/workspaces/[id]/todos/*`) → DB helpers (`src/lib/db/queries.ts`) → SQLite (`workspace_todos` table in the per-user DB at `~/.spaces/<user>.db`).

Mutations are optimistic client-side with rollback on failure. Workspace switch triggers a fresh refetch keyed on `workspaceId`.

## Key files

| Path | Role |
|---|---|
| `src/components/workspace/workspace-todos.tsx` | Inline UI component — fetching, optimistic CRUD, rendering |
| `src/app/api/workspaces/[id]/todos/route.ts` | `GET` list, `POST` create |
| `src/app/api/workspaces/[id]/todos/[todoId]/route.ts` | `PATCH` update, `DELETE` remove |
| `src/lib/db/schema.ts` | `workspace_todos` table declaration inside `initSchema()` |
| `src/lib/db/queries.ts` | `getWorkspaceTodos`, `addWorkspaceTodo`, `updateWorkspaceTodo`, `deleteWorkspaceTodo` |
| `src/types/claude.ts` | `WorkspaceTodo` type |
| `src/app/(desktop)/terminal/page.tsx` | Mounts `<WorkspaceTodos>` above the pane grid |
| `tests/db/workspace-todos.test.ts` | Query-layer tests — ordering, cascade, scoping |

## Data model

### `workspace_todos`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | autoincrement |
| `workspace_id` | INTEGER NOT NULL | FK → `workspaces(id)` ON DELETE CASCADE |
| `text` | TEXT NOT NULL | trimmed on insert/update; empty rejected |
| `completed` | INTEGER NOT NULL DEFAULT 0 | 0 or 1 |
| `sort_order` | INTEGER NOT NULL DEFAULT 0 | meaningful only among uncompleted rows |
| `created` | TEXT NOT NULL DEFAULT datetime('now') | |
| `completed_at` | TEXT | set when completed flips 0→1, cleared on 1→0 |

Index: `idx_workspace_todos_workspace` on `workspace_id`.

### TypeScript

```ts
interface WorkspaceTodo {
  id: number;
  workspaceId: number;
  text: string;
  completed: boolean;
  sortOrder: number;
  created: string;
  completedAt: string | null;
}
```

### Invariants

- Listing order: uncompleted first by `sort_order` ASC, then completed by `completed_at` DESC.
- New todos get `sort_order = MAX(sort_order WHERE completed=0) + 1`.
- `completed_at` is `NULL` iff `completed = 0`.
- Deleting a workspace deletes its todos via FK cascade.

## APIs / contracts

### Internal endpoints

| Method | Path | Request | Response |
|---|---|---|---|
| `GET` | `/api/workspaces/:id/todos` | — | `WorkspaceTodo[]` |
| `POST` | `/api/workspaces/:id/todos` | `{ text: string }` | `201 WorkspaceTodo` or `400 { error }` |
| `PATCH` | `/api/workspaces/:id/todos/:todoId` | `{ text?, completed? }` | `WorkspaceTodo` or `400`/`404` |
| `DELETE` | `/api/workspaces/:id/todos/:todoId` | — | `{ ok: true }` |

Auth via `getAuthUser` + `withUser`, same pattern as sibling workspace routes.

### External calls
N/A.

## Configuration & environment
None. Always on; no env vars or flags.

## How to run / test locally

```bash
npm install
npm run build
npm start
# visit http://localhost:3457/terminal, enter any workspace
```

Automated:
```bash
npx vitest run tests/db/workspace-todos.test.ts
```

## Known quirks & decisions

- **Schema lives in `initSchema()`, no migration file.** Matches the pattern in `src/lib/db/schema.ts`; the repo has no separate migration system.
- **No agent write path in v1.** The coworker's "pi /todos" plugin that inspired this lets agents add todos from inside the session — deferred because it requires an MCP tool or CLI surface.
- **Optimistic updates without toasts.** Failures `console.error` and revert local state; this is a local-only app, so silent revert is acceptable for v1.
- **`sort_order = Number.MAX_SAFE_INTEGER` for optimistic inserts.** The temp row sorts to the bottom of uncompleted; the server returns the real `sort_order` on response and replaces the temp.
- **No confirm on delete.** Minimal ethos. Add undo toast in a follow-up if this proves painful.
- **Direct-to-main workflow.** Per repo convention for this codebase; no PR flow.

## Open questions / TODOs

- [ ] Agent write access via MCP tool once UI v1 proves out.
- [ ] Optional: persist the "show completed" toggle in localStorage.
- [ ] Optional: keyboard shortcut to focus the add-input from anywhere on `/terminal`.

## Changelog

- 2026-04-21 — Initial implementation. v1 ships direct to `main`.
