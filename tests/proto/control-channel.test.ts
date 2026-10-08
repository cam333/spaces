import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { WebSocketServer, WebSocket } from 'ws';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import { attachControlChannel } from '../../bin/lib/runtime/control-channel.js';

let wss: WebSocketServer;
let port: number;
let runtime: ReturnType<typeof createRuntime>;

beforeEach(async () => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  runtime = createRuntime({ nodeId: 'test', spacesDbPath: '' });
  wss = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  wss.on('connection', ws => attachControlChannel(ws, runtime, { authenticated: true }));
  await new Promise(r => wss.once('listening', r));
  port = (wss.address() as any).port;
});

afterEach(() => new Promise(r => wss.close(r)));

function connect(): Promise<WebSocket> {
  const ws = new WebSocket(`ws://127.0.0.1:${port}/proto`);
  return new Promise(res => ws.once('open', () => res(ws)));
}
const recv = (ws: WebSocket) => new Promise<any>(res => ws.once('message', d => res(JSON.parse(d.toString()))));
const send = (ws: WebSocket, obj: unknown) => ws.send(JSON.stringify(obj));

async function waitFor(cond: () => boolean, timeoutMs = 1000): Promise<void> {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > timeoutMs) throw new Error('waitFor timed out');
    await new Promise(r => setTimeout(r, 10));
  }
}

describe('control channel', () => {
  it('hello handshake returns hello-ok with current seq', async () => {
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    const reply = await recv(ws);
    expect(reply).toMatchObject({ type: 'hello-ok', protocolVersion: 1, nodeId: 'test' });
    ws.close();
  });

  it('version mismatch gets a typed error', async () => {
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 99, client: 'test' });
    const reply = await recv(ws);
    expect(reply.error.code).toBe('version_mismatch');
  });

  it('dispatches requests after handshake', async () => {
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    send(ws, { id: 'r1', method: 'session.snapshot', params: {} });
    const reply = await recv(ws);
    expect(reply.id).toBe('r1');
    expect(reply.result.protocolVersion).toBe(1);
    ws.close();
  });

  it('events.subscribe returns backlog then pushes live events', async () => {
    runtime.state.open('p1', { agentType: 'shell', cwd: '.', workspaceId: 'w1' }); // seq 1
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    send(ws, { id: 'r1', method: 'events.subscribe', params: { afterSeq: 0 } });
    const backlog = await recv(ws);
    expect(backlog.result.gap).toBe(false);
    expect(backlog.result.events.map((e: any) => e.type)).toEqual(['pane.opened']);
    const pushP = recv(ws);
    runtime.state.setAgentState('p1', 'working');
    const push = await pushP;
    expect(push.event.type).toBe('pane.state');
    expect(push.event.payload.agentState).toBe('working');
    ws.close();
  });

  it('events.subscribe rejects a non-integer/negative afterSeq instead of silently desyncing (M3)', async () => {
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);

    // Number('not-a-number') is NaN — previously eventsAfter(NaN) returned
    // an empty backlog with gap:false, telling the client it's fully caught
    // up when it isn't.
    send(ws, { id: 'r1', method: 'events.subscribe', params: { afterSeq: 'not-a-number' } });
    expect((await recv(ws)).error.code).toBe('bad_request');

    send(ws, { id: 'r2', method: 'events.subscribe', params: { afterSeq: -1 } });
    expect((await recv(ws)).error.code).toBe('bad_request');

    send(ws, { id: 'r3', method: 'events.subscribe', params: { afterSeq: 1.5 } });
    expect((await recv(ws)).error.code).toBe('bad_request');

    // A valid afterSeq still works (no regression).
    send(ws, { id: 'r4', method: 'events.subscribe', params: { afterSeq: 0 } });
    const ok = await recv(ws);
    expect(ok.id).toBe('r4');
    expect(ok.result.gap).toBe(false);
    ws.close();
  });

  it('requests before hello are rejected', async () => {
    const ws = await connect();
    send(ws, { id: 'r1', method: 'session.snapshot', params: {} });
    const reply = await recv(ws);
    expect(reply.error.code).toBe('bad_request');
  });

  it('a socket error mid-session does not crash the server; other connections keep working', async () => {
    let serverWs: any;
    wss.once('connection', (ws) => { serverWs = ws; });

    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    await waitFor(() => !!serverWs);

    // If attachControlChannel didn't register an 'error' listener before any
    // early return, this emit throws synchronously (Node's default behavior
    // for an 'error' event with zero listeners) and the process itself would
    // crash before this test could finish — so simply reaching the
    // assertions below is part of the proof.
    serverWs.emit('error', new Error('simulated socket error'));
    ws.close();

    // The server (and its process) must still be alive and accepting new
    // control-channel connections.
    const ws2 = await connect();
    send(ws2, { type: 'hello', protocolVersion: 1, client: 'test2' });
    const reply = await recv(ws2);
    expect(reply).toMatchObject({ type: 'hello-ok', protocolVersion: 1, nodeId: 'test' });
    ws2.close();
  });

  it('ws.terminate() fires close and unsubscribes from the event hub', async () => {
    const listeners = () => (runtime.hub as any)._listeners.size;
    // Baseline, not a literal 0/1: createRuntime installs its own permanent
    // hub subscriber (the notifier), independent of any WS connection.
    const baseline = listeners();

    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    send(ws, { id: 'r1', method: 'events.subscribe', params: { afterSeq: 0 } });
    await recv(ws); // backlog reply

    expect(listeners()).toBe(baseline + 1);

    ws.terminate();
    await waitFor(() => listeners() === baseline);
  });

  it('detect.update returns an async refresh result', async () => {
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    send(ws, { id: 'u1', method: 'detect.update', params: {} });
    const reply = await recv(ws);
    expect(reply.id).toBe('u1');
    expect(reply.result).toHaveProperty('updated');
    expect(reply.result).toHaveProperty('skipped');
    ws.close();
  });

  it('agent.wait resolves over the control channel', async () => {
    runtime.state.open('pw1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    send(ws, { id: 'w1', method: 'agent.wait', params: { paneId: 'pw1', until: ['blocked'], timeoutMs: 3000 } });
    setTimeout(() => runtime.state.setAgentState('pw1', 'blocked'), 50);
    const reply = await recv(ws);
    expect(reply.id).toBe('w1');
    expect(reply.result).toMatchObject({ ok: true, state: 'blocked' });
    ws.close();
  });

  it('agent.wait reports a timeout as a typed error', async () => {
    runtime.state.open('pw2', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    const ws = await connect();
    send(ws, { type: 'hello', protocolVersion: 1, client: 'test' });
    await recv(ws);
    send(ws, { id: 'w2', method: 'agent.wait', params: { paneId: 'pw2', until: ['blocked'], timeoutMs: 100 } });
    const reply = await recv(ws);
    expect(reply.error.code).toBe('conflict');
    expect(reply.error.message).toMatch(/^timeout:/);
    ws.close();
  });
});
