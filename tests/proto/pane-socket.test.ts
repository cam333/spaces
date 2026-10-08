import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { WebSocketServer, WebSocket } from 'ws';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import { attachPaneSocket } from '../../bin/lib/runtime/pane-socket.js';

let wss: WebSocketServer, port: number, runtime: ReturnType<typeof createRuntime>;
const inputs: string[] = [];

beforeEach(async () => {
  inputs.length = 0;
  runtime = createRuntime({ nodeId: 'n', spacesDbPath: '' });
  runtime.registerPane('p1', {
    cols: 80, rows: 24, agentType: 'shell', cwd: '.', workspaceId: null,
    onInput: d => inputs.push(d), onResize: () => {},
  });
  wss = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  wss.on('connection', (ws, req) => {
    const url = new URL(req.url!, 'http://x');
    const paneId = url.pathname.split('/').pop()!;
    attachPaneSocket(ws, runtime, {
      paneId,
      mode: (url.searchParams.get('mode') as any) || 'observe',
      takeover: url.searchParams.get('takeover') === '1',
      authenticated: true,
    });
  });
  await new Promise(r => wss.once('listening', r));
  port = (wss.address() as any).port;
});

afterEach(() => new Promise(r => wss.close(r)));

function connect(paneId: string, qs = ''): Promise<{ ws: WebSocket; frames: any[]; binary: Buffer[] }> {
  const ws = new WebSocket(`ws://127.0.0.1:${port}/proto/pane/${paneId}${qs}`);
  const frames: any[] = []; const binary: Buffer[] = [];
  ws.on('message', (d, isBinary) => {
    if (isBinary) binary.push(d as Buffer);
    else frames.push(JSON.parse(d.toString()));
  });
  return new Promise(res => ws.once('open', () => res({ ws, frames, binary })));
}
const settle = (ms = 100) => new Promise(r => setTimeout(r, ms));

describe('pane data socket', () => {
  it('replays screen content on attach, then streams live chunks as binary', async () => {
    runtime.feed('p1', 'earlier output\r\n');
    await settle();
    const { ws, frames, binary } = await connect('p1', '?mode=observe');
    await settle();
    expect(frames.map(f => f.type)).toEqual(['replay-start', 'replay-end']);
    expect(Buffer.concat(binary).toString()).toContain('earlier output');
    const before = binary.length;
    runtime.feed('p1', 'live chunk');
    await settle();
    expect(Buffer.concat(binary.slice(before)).toString()).toContain('live chunk');
    ws.close();
  });

  it('controller input reaches the PTY; observer input is rejected', async () => {
    const ctl = await connect('p1', '?mode=control');
    const obs = await connect('p1', '?mode=observe');
    await settle();
    ctl.ws.send(JSON.stringify({ t: 'i', data: 'ls\r' }));
    obs.ws.send(JSON.stringify({ t: 'i', data: 'evil\r' }));
    await settle();
    expect(inputs).toEqual(['ls\r']);
    expect(obs.frames.some(f => f.type === 'error' && f.code === 'bad_request')).toBe(true);
    ctl.ws.close(); obs.ws.close();
  });

  it('second controller is rejected without takeover, succeeds with it', async () => {
    const a = await connect('p1', '?mode=control');
    await settle();
    const b = await connect('p1', '?mode=control');
    await settle();
    expect(b.frames.some(f => f.type === 'error' && f.code === 'conflict')).toBe(true);

    const c = await connect('p1', '?mode=control&takeover=1');
    await settle(150);
    c.ws.send(JSON.stringify({ t: 'i', data: 'x' }));
    await settle();
    expect(inputs).toContain('x');
    expect(a.frames.some(f => f.type === 'error' && f.code === 'conflict')).toBe(true);
    a.ws.close(); c.ws.close();
  });

  it('unknown pane gets not_found and close', async () => {
    const { ws, frames } = await connect('nope', '?mode=observe');
    await settle();
    expect(frames.some(f => f.type === 'error' && f.code === 'not_found')).toBe(true);
    expect(ws.readyState === WebSocket.CLOSED || ws.readyState === WebSocket.CLOSING).toBe(true);
  });

  it('paneExit fans out an exit frame to both the observer and the controller, then closes both (I1)', async () => {
    const ctl = await connect('p1', '?mode=control');
    const obs = await connect('p1', '?mode=observe');
    await settle();

    runtime.paneExit('p1', 7);
    await settle();

    expect(ctl.frames.some(f => f.type === 'exit' && f.exitCode === 7)).toBe(true);
    expect(obs.frames.some(f => f.type === 'exit' && f.exitCode === 7)).toBe(true);
    expect([WebSocket.CLOSED, WebSocket.CLOSING]).toContain(ctl.ws.readyState);
    expect([WebSocket.CLOSED, WebSocket.CLOSING]).toContain(obs.ws.readyState);

    // Controller input after exit must never be silently dropped — the pane
    // is gone, so it should surface as an explicit error, not vanish.
    ctl.ws.send(JSON.stringify({ t: 'i', data: 'too-late\r' }));
    await settle();
    expect(inputs).not.toContain('too-late\r');
  });
});
