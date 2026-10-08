# Project Wizard ("Plan a Project")

**Status:** beta
**Owner:** Spaces core
**Last updated:** 2026-04-17

## Overview

A conversational wizard that turns a plain-English project description into a ready-to-launch Spaces workspace with pre-configured agent panes. The user chats with Claude Haiku; the model returns both conversational replies and a structured `ProjectPlan` JSON block that the UI renders and lets the user edit before launch. On launch, the wizard creates a workspace row and one pane per entry in the plan, then switches the user into it.

**In scope:** initial workspace scaffolding (name, color, description) and pane set (agent type, cwd, title, optional custom command / model).
**Not in scope:** executing the `initialPrompt` inside each pane after launch (currently stored in the plan but not injected into the started agent), long-running plan persistence across sessions, and multi-turn plan diffs.

## User-facing behavior

- **Entry points:**
  - Workspace chooser (pre-entry): "Plan a Project" button — `src/app/(desktop)/terminal/page.tsx:634-639`
  - Workspace switcher dropdown (post-entry): "Plan Project" button — `src/app/(desktop)/terminal/page.tsx:840-845`
- **Flow:**
  1. Wizard opens full-screen. Left pane is a chat; right pane is a live plan summary.
  2. User describes the project. Haiku asks 1–2 clarifying questions, updating the plan JSON each turn.
  3. When the plan looks right, user clicks **Review**.
  4. Review screen lets the user edit workspace fields and each pane's agent type, title, cwd, prompt, and custom command.
  5. **Launch** creates the workspace and panes via existing APIs and drops the user into the new workspace.
- **Exit:** X button in header. No confirmation — the plan is ephemeral until launch.

## Architecture

```
Wizard UI (React, client)
  src/components/wizard/project-wizard.tsx          (shell + phase state)
   ├─ wizard-chat.tsx                               (message list + input)
   ├─ wizard-plan-summary.tsx                       (live plan preview)
   └─ wizard-review.tsx                             (edit + launch)
            │
            │ POST /api/wizard/chat { messages, currentPlan }
            ▼
API route (Next.js, server)
  src/app/api/wizard/chat/route.ts
   - reads the Anthropic key from the environment
   - calls https://api.anthropic.com/v1/messages (Haiku 4.5)
   - extracts the ```json``` block from the reply → returns { reply, plan }
            │
            │ on Launch:
            │   POST /api/workspaces  (create workspace)
            │   POST /api/panes       (one per plan.panes, loop)
            ▼
Existing workspace / pane APIs
```

The wizard owns no DB tables of its own. It is a client-side orchestrator that talks to one dedicated LLM endpoint and then fans out to existing workspace/pane APIs on launch.

## Key files

| Path | Role |
|---|---|
| `src/components/wizard/project-wizard.tsx` | Top-level modal. Owns `messages`, `plan`, `phase` state. Calls `/api/wizard/chat` and the launch APIs. |
| `src/components/wizard/wizard-chat.tsx` | Chat message list + input box. Stateless w.r.t. plan. |
| `src/components/wizard/wizard-plan-summary.tsx` | Right-side live preview of the current plan, "Review" CTA. |
| `src/components/wizard/wizard-review.tsx` | Full-screen edit view: workspace + each pane. Calls `onLaunch`. |
| `src/app/api/wizard/chat/route.ts` | Server handler. Holds the system prompt, calls Anthropic, extracts the JSON plan. |
| `src/app/(desktop)/terminal/page.tsx` | Consumer. Renders `<ProjectWizard>`, handles `onLaunch` → `switchWorkspace(wsId)`. |

## Data model

The wizard is stateless on the server. The only durable artifacts are the workspace and panes created at launch, which use the existing schemas.

**TypeScript contract** (`project-wizard.tsx:10-26`):

```ts
interface ProjectPlan {
  workspace: { name: string; description: string; color: string };
  panes: Array<{
    title: string;
    agentType: 'claude' | 'codex' | 'gemini' | 'aider' | 'forge' | 'shell' | 'custom';
    cwd: string;
    initialPrompt?: string;   // stored in plan, NOT currently delivered to the pane on launch
    customCommand?: string;   // required iff agentType === 'custom'
    customModelId?: string;
    description: string;
  }>;
  summary: string;
}
```

**Invariants:**
- `plan.panes.length` is expected to be 2–5 per the system prompt; not enforced in code.
- `agentType === 'custom'` without `customCommand` will create a broken pane (the pane API accepts it; the pane will fail to start). The Add Pane dialog validates this, the wizard review does not — see Known quirks.

## APIs / contracts

### Internal endpoints

**`POST /api/wizard/chat`** — `src/app/api/wizard/chat/route.ts`

- **Auth:** `getAuthUser(request)` + `withUser(user, ...)`. Same auth as the rest of the app.
- **Request body:**
  ```json
  { "messages": [{"role": "user"|"assistant", "content": "..."}], "currentPlan": <ProjectPlan | null> }
  ```
- **Response:**
  - 200 `{ "reply": "<assistant text, JSON block stripped>", "plan": <ProjectPlan | null> }`
  - 400 `{ "error": "No API key configured..." }` when no Anthropic key is available
  - 502 `{ "error": "API error: <status>" }` when Anthropic call fails
  - 500 `{ "error": "<message>" }` on uncaught errors (e.g., 30s timeout)
- **Notes:** `currentPlan` is appended to the system prompt each turn so the model can mutate it incrementally.

**Launch path** — uses existing endpoints:
- `POST /api/workspaces` with `{ name, description, color }` → `{ id }`
- `POST /api/panes` per plan entry with `{ title, agentType, cwd, customCommand, workspaceId, customModelId }`

### External calls

- **Anthropic Messages API** — `https://api.anthropic.com/v1/messages`, model `claude-haiku-4-5-20251001`, `max_tokens: 2048`, `AbortSignal.timeout(30_000)`.
  - Key source: `process.env.ANTHROPIC_API_KEY`.
  - Haiku was chosen for cost + latency; the structured JSON contract is simple enough that Haiku handles it reliably.

## Configuration & environment

| Name | Required | Purpose |
|---|---|---|
| `ANTHROPIC_API_KEY` (env) | Required | The wizard's Anthropic key. |

The wizard has no tier gate — it is available to all users. Compare with `hasCollaboration` / `hasNetwork` which gate other buttons in the same chooser.

## How to run / test locally

```bash
# 1. Install + run the app
npm install
npm run dev   # Next.js on http://localhost:3000

# 2. Set an Anthropic key (pick one):
export ANTHROPIC_API_KEY=sk-ant-...

# 3. Open the terminal page and click "Plan a Project"
#    http://localhost:3000/terminal
```

**Manual test script:**
1. Click **Plan a Project**.
2. Type: *"Next.js app with a Postgres backend. I want one agent working on the API and one on the frontend."*
3. Verify the right-hand summary populates with 2 panes within a few seconds.
4. Click **Review**. Rename a pane, change an agent type, hit **Launch**.
5. Confirm you land in a new workspace with the configured panes.

**Regression checks:**
- No API key set → user sees the "No API key configured" error inline, not a blank response.
- Anthropic returns non-200 → 502 bubbles up as a red banner at the top of the wizard.
- Closing mid-chat → state is discarded. Reopening starts fresh.

## Known quirks & decisions

- **`initialPrompt` is stored but not delivered.** The plan carries `initialPrompt` per pane, and the system prompt asks the model to fill it in, but the launch code (`project-wizard.tsx:89-102`) does not forward it to `/api/panes`, and the pane create API has no way to inject a first message into the agent's stdin. This is a known gap — see Open questions.
- **Plan is extracted from a ` ```json ` code block in the assistant reply**, then the block is stripped from the text the user sees (`route.ts:98-106`). If the model emits malformed JSON, the old plan is kept silently and the user sees only prose. This is intentional — we'd rather show a normal chat reply than an error — but it means "the plan didn't update" can be a silent failure. Check the raw Anthropic response when debugging.
- **Model choice is hard-coded to `claude-haiku-4-5-20251001`** in `route.ts:80`. If you bump the SDK / model catalog, update this string. There is no automatic fallback.
- **30-second timeout** on the Anthropic call (`route.ts:85`). Haiku is fast so this is usually plenty, but very long conversation histories can approach it. We do not truncate / summarize history.
- **Custom-command panes are not validated in the review step** — the wizard will happily launch an `agentType: 'custom'` pane with an empty `customCommand`, producing a dead pane. The **Add Pane** dialog in the main terminal UI does validate (`terminal/page.tsx:1178`); the wizard review should too.
- **Dual mount points.** `<ProjectWizard>` is rendered both inside the pre-entry chooser branch and inside the main workspace UI (`terminal/page.tsx:721-728` and `1320-1327`). Both instances share the `showWizard` state. This was added in commit `907febb` ("wire ProjectWizard into terminal page"); before that, only the pre-entry branch had it and the switcher button was dead.
- **No streaming.** The chat response is one-shot, so the user stares at a spinner for 1–3s per turn. We accepted this because streaming + JSON-block extraction is fiddly, and Haiku latency is tolerable.

## Open questions / TODOs

- [ ] Deliver `initialPrompt` to the created pane so the agent starts working immediately. Likely path: accept `initialPrompt` in `POST /api/panes`, have the terminal server write it to the pty once the agent signals ready.
- [ ] Validate `agentType === 'custom'` panes in the review step (require `customCommand`).
- [ ] Persist in-progress wizard chats across refresh (localStorage) so a dropped connection doesn't lose context.
- [ ] Expose the model choice as config — some teams will want Sonnet for higher-quality plans.
- [ ] Stream the assistant reply; render the JSON block incrementally so the plan preview updates token-by-token.
- [ ] Telemetry: we track `workspace_created` on launch but not `wizard_opened` / `wizard_launched` / `wizard_abandoned`. Add these to understand funnel.

## Changelog

- **2026-04-17** — Wired `<ProjectWizard>` into the post-entry workspace UI (`907febb`). Previously only reachable from the pre-entry chooser.
