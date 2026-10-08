import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import http from 'node:http';
import { WebSocket } from 'ws';

// The legacy /ws contract, exercised end-to-end through createTerminalServer.
// Locks in: ready frame, live output, input round-trip, resize, exit frame.
let server: http.Server;
let port: number;

// The owner's secret, which every desktop-mode credential derives from.
const SECRET = 'e'.repeat(64);

beforeEach(async () => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  process.env.SPACES_INTERNAL_TOKEN = SECRET;
  const { createTerminalServer } = require('../../bin/terminal-server.js');
  server = http.createServer((_req, res) => { res.writeHead(404); res.end(); });
  createTerminalServer(server);
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()));
  port = (server.address() as any).port;
});

afterEach(() => new Promise<void>(r => server.close(() => r())));

// handleConnection rejects any /ws upgrade that carries no valid credential.
// In desktop mode that means the owner's: here the secret as a bearer, which
// is what an agent or tool on the owner's account presents.
function connect(query: string) {
  const ws = new WebSocket(`ws://127.0.0.1:${port}/ws?${query}`, {
    headers: { Authorization: `Bearer ${SECRET}` },
  });
  const frames: any[] = [];
  ws.on('message', (d) => {
    try { frames.push(JSON.parse(d.toString())); } catch { frames.push({ raw: d.toString() }); }
  });
  return { ws, frames, open: new Promise<void>(r => ws.once('open', () => r())) };
}

const waitFor = async (pred: () => boolean, ms = 8000) => {
  const start = Date.now();
  while (!pred()) {
    if (Date.now() - start > ms) throw new Error('timeout waiting for condition');
    await new Promise(r => setTimeout(r, 25));
  }
};

describe('legacy /ws contract', () => {
  it('sends a ready frame naming the pane', async () => {
    const paneId = `parity-${Date.now()}`;
    const c = connect(`paneId=${paneId}&cwd=${encodeURIComponent(process.cwd())}&agentType=shell&cols=80&rows=24`);
    await c.open;
    await waitFor(() => c.frames.some(f => f.type === 'ready'));
    expect(c.frames.find(f => f.type === 'ready')).toMatchObject({ type: 'ready', paneId });
    c.ws.close();
  }, 15000);

  it('streams shell output and accepts input', async () => {
    const paneId = `parity-io-${Date.now()}`;
    const c = connect(`paneId=${paneId}&cwd=${encodeURIComponent(process.cwd())}&agentType=shell&cols=80&rows=24`);
    await c.open;
    await waitFor(() => c.frames.some(f => f.type === 'ready'));
    const marker = `PARITY_${Date.now()}`;
    c.ws.send(JSON.stringify({ type: 'data', data: `echo ${marker}\r` }));
    await waitFor(() => c.frames.some(f => f.type === 'data' && String(f.data).includes(marker)));
    c.ws.close();
  }, 20000);

  it('accepts a resize message without erroring', async () => {
    const paneId = `parity-resize-${Date.now()}`;
    const c = connect(`paneId=${paneId}&cwd=${encodeURIComponent(process.cwd())}&agentType=shell&cols=80&rows=24`);
    await c.open;
    await waitFor(() => c.frames.some(f => f.type === 'ready'));
    c.ws.send(JSON.stringify({ type: 'resize', cols: 120, rows: 40 }));
    await new Promise(r => setTimeout(r, 300));
    expect(c.frames.some(f => f.type === 'error')).toBe(false);
    c.ws.close();
  }, 15000);

  it('reports process exit with an exit frame', async () => {
    const paneId = `parity-exit-${Date.now()}`;
    const c = connect(`paneId=${paneId}&cwd=${encodeURIComponent(process.cwd())}&agentType=shell&cols=80&rows=24`);
    await c.open;
    await waitFor(() => c.frames.some(f => f.type === 'ready'));
    c.ws.send(JSON.stringify({ type: 'data', data: 'exit\r' }));
    await waitFor(() => c.frames.some(f => f.type === 'exit'), 12000);
    expect(c.frames.find(f => f.type === 'exit')).toHaveProperty('exitCode');
    c.ws.close();
  }, 20000);

  it('replays buffered output to a reattaching client', async () => {
    const paneId = `parity-reattach-${Date.now()}`;
    const first = connect(`paneId=${paneId}&cwd=${encodeURIComponent(process.cwd())}&agentType=shell&cols=80&rows=24`);
    await first.open;
    await waitFor(() => first.frames.some(f => f.type === 'ready'));
    const marker = `REATTACH_${Date.now()}`;
    first.ws.send(JSON.stringify({ type: 'data', data: `echo ${marker}\r` }));
    await waitFor(() => first.frames.some(f => f.type === 'data' && String(f.data).includes(marker)));
    first.ws.close();
    await new Promise(r => setTimeout(r, 200));

    const second = connect(`paneId=${paneId}&cwd=${encodeURIComponent(process.cwd())}&agentType=shell&cols=80&rows=24`);
    await second.open;
    await waitFor(() => second.frames.some(f => f.type === 'ready'));
    expect(second.frames.find(f => f.type === 'ready')).toMatchObject({ reattached: true });
    await waitFor(() => second.frames.some(f => f.type === 'data' && String(f.data).includes(marker)));
    second.ws.close();
  }, 25000);
});
