# Spaces — guide for coding agents

Spaces is an agent workspace manager: a server that runs terminal panes in
workspaces, tracks each agent's state (working / blocked / idle / done), and
serves web, TUI, and VR clients. This guide is for a coding agent running
*inside* a Spaces pane.

**Guardrail:** only follow this guide when `SPACES_PANE_ID` is set in your
environment. If it is not set, you are not running inside a Spaces pane —
stop and ignore everything below.

## Your environment

Every Spaces pane gets these variables:

| Variable | Meaning |
| --- | --- |
| `SPACES_PANE_ID` | This pane's id — proof you are inside Spaces |
| `SPACES_WORKSPACE_ID` | The workspace this pane belongs to |
| `SPACES_PANE_NAME` | This pane's display name (your sender name on the bus) |
| `SPACES_API_URL` | Base URL of the local Spaces server API |
| `SPACES_BASE_PATH` | Path prefix when Spaces sits behind a reverse proxy (often empty) |

## Agent state is automatic

Spaces reads your terminal output and OSC title/progress reports to classify
you as working, blocked, idle, or done, and notifies the human when you need
attention. You do not need to report state — just work normally. Interactive
prompts (permission dialogs, selection menus) are detected and surfaced.

## Talking to other agents and the human: `spaces-msg`

`spaces-msg` is on PATH inside every pane. It posts to the workspace message
bus that all panes and the human share.

```
spaces-msg post <text>              Post a message to the workspace
spaces-msg post --to <name> <text>  Address one agent by pane name
spaces-msg request <text>           Ask the human for something (held for approval)
spaces-msg read                     Read workspace messages
spaces-msg read --since 15m         Limit to a window (Nm, Nh, Nd)
spaces-msg agents                   List the agent panes in this workspace
spaces-msg context get [key]        Read shared workspace context
spaces-msg context set <key> <val>  Publish shared workspace context
spaces-msg context delete <key>     Remove a context entry
```

Use `context` for durable facts other agents need (ports, decisions, file
ownership); use `post` for conversation. `request` is the escalation path —
it waits for human approval, so reserve it for things you genuinely cannot
proceed without.

The same bus is exposed to MCP-capable agents as the `spaces-bus` server
(tools: `post_message`, `read_messages`, `list_agents`, `get_context`,
`set_context`, `request_help`).

## Printing this guide

`spaces guide` prints this document from any install.
