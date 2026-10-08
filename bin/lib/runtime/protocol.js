'use strict';

const { z } = require('zod');

const PROTOCOL_VERSION = 1;

const ERROR_CODES = [
  'bad_request', 'unauthorized', 'not_found', 'conflict',
  'version_mismatch', 'gap', 'internal',
];

const AgentState = z.enum(['working', 'blocked', 'idle', 'unknown']);

const SessionRef = z.object({
  id: z.string(),
  transcriptPath: z.string().nullable(),
}).nullable();

const PaneInfo = z.object({
  id: z.string(),
  workspaceId: z.string().nullable(),
  agentType: z.string(),
  agentState: AgentState,
  seen: z.boolean(),
  title: z.string(),
  cwd: z.string(),
  sessionRef: SessionRef,
  live: z.boolean(),
  // The pane's accent color, `#rrggbb`, or null when it has never been given
  // one. Persisted alongside `title` in the panes table (both NOT NULL in
  // production, `src/lib/db/schema.ts`) and set through `pane.update`.
  color: z.string().nullable(),
});

// The working copy behind a space: its first pane's directory. `branch` is
// null on a detached HEAD; `ahead`/`behind` are 0 with no upstream.
const GitStatus = z.object({
  branch: z.string().nullable(),
  ahead: z.number().int().nonnegative(),
  behind: z.number().int().nonnegative(),
  dirty: z.boolean(),
});

const WorkspaceInfo = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string().nullable(),
  paneIds: z.array(z.string()),
  // Absent from older servers; null when the space is not in a repository
  // or has not been looked at yet. Changes arrive as `workspace.updated`.
  git: GitStatus.nullable().optional(),
});

/**
 * One machine in the fleet, as the client sees it.
 *
 * Carries no key material: `paired` is a boolean and `keyHint` is a truncated
 * public key, enough to tell two apart in a list and useless for anything else.
 * The client never receives a credential — the local node holds those and does
 * the connecting, so the thing with the widest reach has the least to steal.
 */
const NetNode = z.object({
  nodeId: z.string(),
  name: z.string(),
  url: z.string(),
  // 'connecting' | 'online' | 'offline' | 'unpaired' — a string rather than an
  // enum so an older client renders a state a newer server invented instead of
  // failing to parse the frame that carries it.
  status: z.string(),
  paired: z.boolean(),
  discoveredVia: z.string(),
  version: z.string().nullable(),
  lastSeen: z.string().nullable(),
  keyHint: z.string().nullable(),
  /** True for the machine this client is directly connected to. */
  self: z.boolean(),
});

/**
 * One past agent session, as offered to a client choosing what a new pane
 * should start as.
 *
 * `label` is resolved server-side — custom name, then summary, then the first
 * prompt, then the id — so no client has to reimplement a four-step
 * precedence, and every client shows the same thing.
 */
const SessionSummary = z.object({
  sessionId: z.string(),
  label: z.string(),
  messageCount: z.number().int().nonnegative(),
  modified: z.string().nullable(),
  gitBranch: z.string().nullable(),
  agentType: z.string(),
});

const Snapshot = z.object({
  protocolVersion: z.literal(PROTOCOL_VERSION),
  nodeId: z.string(),
  seq: z.number().int().nonnegative(),
  workspaces: z.array(WorkspaceInfo),
  panes: z.array(PaneInfo),
});

const Hello = z.object({
  type: z.literal('hello'),
  protocolVersion: z.number().int(),
  client: z.string(),
});

const HelloOk = z.object({
  type: z.literal('hello-ok'),
  protocolVersion: z.literal(PROTOCOL_VERSION),
  nodeId: z.string(),
  seq: z.number().int().nonnegative(),
});

const Request = z.object({
  id: z.string().min(1),
  method: z.string().min(1),
  // Two-arg z.record is valid in zod v3 AND v4; the one-arg form throws under
  // v4, and this tree can hoist either major (the MCP SDK accepts both).
  params: z.record(z.string(), z.unknown()).optional(),
});

const OkResponse = z.object({ id: z.string(), result: z.unknown() });

const ErrResponse = z.object({
  id: z.string(),
  error: z.object({ code: z.enum(ERROR_CODES), message: z.string() }),
});

const EventFrame = z.object({
  event: z.object({
    seq: z.number().int().positive(),
    type: z.enum([
      'pane.opened', 'pane.closed', 'pane.state', 'pane.seen',
      'pane.title', 'pane.session', 'pane.notify',
      'pane.created', 'pane.removed', 'pane.updated',
      'workspace.created', 'workspace.updated', 'workspace.removed', 'pane.moved',
      // A machine in the fleet changed: paired, forgotten, or its connection
      // state moved. Pushed so the sidebar never polls for it.
      'net.node.updated',
    ]),
    payload: z.record(z.string(), z.unknown()),
    ts: z.number(),
  }),
});

const PaneUpFrame = z.discriminatedUnion('t', [
  z.object({ t: z.literal('i'), data: z.string() }),
  z.object({ t: z.literal('r'), cols: z.number().int().positive(), rows: z.number().int().positive() }),
]);

// Downward wire format sent by pane-socket.js on the pane-data-socket
// (/proto/pane/<id>). Previously these existed only as string literals
// scattered through pane-socket.js — codified here so both the server's own
// sendJson() helper and third-party clients (including the generated JSON
// Schema) have one source of truth for the shape.
const PaneDownFrame = z.discriminatedUnion('type', [
  z.object({ type: z.literal('replay-start') }),
  z.object({ type: z.literal('replay-end') }),
  z.object({ type: z.literal('exit'), exitCode: z.number().int() }),
  z.object({ type: z.literal('error'), code: z.enum(ERROR_CODES), message: z.string() }),
]);

module.exports = {
  PROTOCOL_VERSION,
  ERROR_CODES,
  schemas: {
    AgentState, PaneInfo, WorkspaceInfo, Snapshot, NetNode, SessionSummary,
    Hello, HelloOk, Request, OkResponse, ErrResponse,
    EventFrame, PaneUpFrame, PaneDownFrame,
  },
};
