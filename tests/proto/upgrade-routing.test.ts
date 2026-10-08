// Every other proto test in this directory bypasses the real 'upgrade'
// handler in bin/terminal-server.js — they attach attachControlChannel /
// attachPaneSocket directly to their own WebSocketServer. That's how C1 (an
// unauthenticated, pre-Origin-check process crash via a malformed pane id)
// survived ten reviews: nothing ever drove a real HTTP upgrade through
// createTerminalServer()'s routing, Origin gate, and decodeURIComponent
// call. This file boots a real http.Server, mounts createTerminalServer()
// on it, and drives genuine WebSocket upgrades against it.
import { describe, it, expect, beforeAll, afterAll, vi } from 'vitest';
import http from 'node:http';
import { WebSocket } from 'ws';
import { createTerminalServer, getRuntime } from '../../bin/terminal-server.js';

// Resolves once the given WS upgrade is rejected (socket destroyed / reset
// before a 101 response, or a non-101 HTTP response). Rejects (test failure)
// if the upgrade unexpectedly succeeds, or if neither happens within the
// timeout.
function expectUpgradeRejected(url: string, headers?: Record<string, string>): Promise<void> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url, headers ? { headers } : undefined);
    const timer = setTimeout(() => {
      ws.terminate();
      reject(new Error(`timed out waiting for upgrade rejection: ${url}`));
    }, 3000);
    const done = () => { clearTimeout(timer); resolve(); };
    ws.once('error', done);
    ws.once('unexpected-response', done);
    ws.once('close', done);
    ws.once('open', () => {
      clearTimeout(timer);
      ws.close();
      reject(new Error(`expected upgrade to be rejected, but it opened: ${url}`));
    });
  });
}

// The owner's secret; see bin/lib/desktop-auth.js.
const SECRET = 'f'.repeat(64);
process.env.SPACES_INTERNAL_TOKEN = SECRET;
const OWNER = { Authorization: `Bearer ${SECRET}` };

function connectAndRecv(url: string, headers: Record<string, string> = OWNER): Promise<{ ws: WebSocket; first: Promise<any> }> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url, { headers });
    const timer = setTimeout(() => { ws.terminate(); reject(new Error(`timed out opening ${url}`)); }, 3000);
    ws.once('open', () => {
      clearTimeout(timer);
      const first = new Promise<any>((res) => {
        ws.once('message', (d, isBinary) => res(isBinary ? d : JSON.parse(d.toString())));
      });
      resolve({ ws, first });
    });
    ws.once('error', reject);
  });
}

describe('real upgrade routing (createTerminalServer)', () => {
  // ── (a), (b), (c): single-user server shared across these cases ──
  let server: http.Server;
  let port: number;

  beforeAll(async () => {
    server = http.createServer((req, res) => { res.statusCode = 404; res.end(); });
    createTerminalServer(server);
    getRuntime().registerPane('probe', {
      cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: null,
      onInput: () => {}, onResize: () => {},
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
    port = (server.address() as any).port;
  });

  afterAll(() => new Promise<void>((resolve) => server.close(() => resolve())));

  it('(C1) a malformed pane id (%zz) does not crash the server; the server keeps serving subsequent connections', async () => {
    // Pre-auth, pre-Origin-check: no Origin header, no token. If
    // decodeURIComponent's URIError escaped the 'upgrade' handler uncaught,
    // ws.handleUpgrade's synchronous callback invocation means the whole
    // process (terminal server + Next.js share it) would die right here —
    // there would be no server left to answer the next connection below.
    await expectUpgradeRejected(`ws://127.0.0.1:${port}/proto/pane/%zz`);

    const ws = new WebSocket(`ws://127.0.0.1:${port}/proto`, { headers: OWNER });
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('subsequent /proto connection timed out')), 3000);
      ws.once('open', () => { clearTimeout(timer); resolve(); });
      ws.once('error', reject);
    });
    const reply = new Promise<any>((res) => ws.once('message', (d) => res(JSON.parse(d.toString()))));
    ws.send(JSON.stringify({ type: 'hello', protocolVersion: 1, client: 'upgrade-routing-test' }));
    expect(await reply).toMatchObject({ type: 'hello-ok', protocolVersion: 1 });
    ws.close();
  });

  it('(routing order) /proto/pane/<id> hits the pane-data-socket route, /proto hits the control channel', async () => {
    // The pane-data-socket route sends a `replay-start` text frame
    // immediately on attach, without waiting for any client message — a
    // control-channel socket never sends anything until it receives (and
    // validates) a `hello`. Getting `replay-start` here proves the
    // /proto/pane/<id> match (checked first, since it shares the /proto
    // prefix) actually won and didn't fall through to the control channel.
    const { ws: paneWs, first: paneFirst } = await connectAndRecv(`ws://127.0.0.1:${port}/proto/pane/probe`);
    expect(await paneFirst).toMatchObject({ type: 'replay-start' });
    paneWs.close();

    // The exact-/proto branch, conversely, only replies to a valid `hello`.
    const { ws: ctrlWs } = await connectAndRecv(`ws://127.0.0.1:${port}/proto`);
    const reply = new Promise<any>((res) => ctrlWs.once('message', (d) => res(JSON.parse(d.toString()))));
    ctrlWs.send(JSON.stringify({ type: 'hello', protocolVersion: 1, client: 'upgrade-routing-test' }));
    expect(await reply).toMatchObject({ type: 'hello-ok', protocolVersion: 1 });
    ctrlWs.close();
  });

  it('(Origin gate) a disallowed Origin is rejected for /proto', async () => {
    await expectUpgradeRejected(`ws://127.0.0.1:${port}/proto`, { Origin: 'http://evil.example.com' });
  });

  it('(Origin gate) a disallowed Origin is rejected for /proto/pane/<id>', async () => {
    await expectUpgradeRejected(`ws://127.0.0.1:${port}/proto/pane/probe`, { Origin: 'http://evil.example.com' });
  });

  // ── (d): multiuser mode, isolated module instance ──
  // IS_MULTIUSER is read from process.env at module-require time (a plain
  // top-level const in terminal-server.js), so the already-imported module
  // above is permanently non-multiuser. Force a fresh module evaluation with
  // the env stubbed to exercise the multiuser branch.
  it('(C2) multiuser mode refuses both /proto and /proto/pane/<id> upgrades', async () => {
    vi.stubEnv('SPACES_MULTIUSER', '1');
    vi.resetModules();
    let muServer: http.Server | undefined;
    try {
      const mod = await import('../../bin/terminal-server.js');
      muServer = http.createServer((req, res) => { res.statusCode = 404; res.end(); });
      mod.createTerminalServer(muServer);
      await new Promise<void>((resolve) => muServer!.listen(0, '127.0.0.1', () => resolve()));
      const muPort = (muServer.address() as any).port;

      // No Origin header, no auth token, loopback address — none of that
      // matters: multiuser mode must refuse these routes before any of it is
      // even consulted (Phase-1 restriction pending per-pane ownership).
      await expectUpgradeRejected(`ws://127.0.0.1:${muPort}/proto`);
      await expectUpgradeRejected(`ws://127.0.0.1:${muPort}/proto/pane/anything`);
    } finally {
      if (muServer) await new Promise<void>((resolve) => muServer!.close(() => resolve()));
      vi.unstubAllEnvs();
      vi.resetModules();
    }
  });
});
