import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import WebSocket from 'ws';
import { loadOrCreateIdentity, publicKeyB64 } from '../../bin/lib/network/identity.js';
import { openNodeDb, pairNode } from '../../bin/lib/network/nodes.js';
import { createFederationListener, routeOf } from '../../bin/lib/network/listener.js';
import { relayPane } from '../../bin/lib/network/pane-relay.js';
import { createAuditLog } from '../../bin/lib/network/audit.js';

describe('federation routing', () => {
  it('recognises the control channel and a pane channel', () => {
    expect(routeOf('/federation')).toEqual({ kind: 'control' });
    expect(routeOf('/federation?x=1')).toEqual({ kind: 'control' });
    expect(routeOf('/federation/pane/p1')).toEqual({ kind: 'pane', paneId: 'p1' });
    expect(routeOf('/federation/pane/p1?mode=control')).toEqual({ kind: 'pane', paneId: 'p1' });
  });

  it('decodes a pane id that needed encoding', () => {
    expect(routeOf('/federation/pane/pane%201')).toEqual({ kind: 'pane', paneId: 'pane 1' });
  });

  it('serves nothing else', () => {
    // Every path that is not one of the two is refused before the handshake.
    for (const p of ['/proto', '/ws', '/', '/federation/pane/', '/federation/pane/a/b', '/federationx']) {
      expect(routeOf(p), p).toBeNull();
    }
  });
});

function machine(label: string) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `spaces-pr-${label}-`));
  return {
    dir,
    identity: loadOrCreateIdentity({ dir }),
    db: openNodeDb({ path: path.join(dir, 'network.db') }),
  };
}

let owner: any;
let viewer: any;
let listener: any;
const sockets: any[] = [];

beforeEach(() => {
  owner = machine('owner');
  viewer = machine('viewer');
});

afterEach(async () => {
  for (const s of sockets.splice(0)) {
    try { s.terminate(); } catch { /* gone */ }
  }
  if (listener) await listener.close();
  listener = null;
});

describe('relaying a pane from another machine', () => {
  /**
   * The owning machine, serving a fake pane that echoes what it is sent and
   * emits one binary chunk up front — enough to prove both directions and
   * that binary frames stay binary.
   */
  async function startOwner() {
    listener = createFederationListener({
      iface: '127.0.0.1',
      port: 0,
      identity: owner.identity,
      db: owner.db,
      audit: createAuditLog({ path: path.join(owner.dir, 'audit.log') }),
      onPeer: (ws: any, _node: any, target: any) => {
        if (target.kind !== 'pane') return;
        ws.send(JSON.stringify({ type: 'replay-start' }));
        ws.send(Buffer.from(`screen of ${target.paneId}`), { binary: true });
        ws.send(JSON.stringify({ type: 'replay-end' }));
        ws.on('message', (raw: any, isBinary: boolean) => {
          // Echo input back as PTY output, the way a shell would.
          if (!isBinary) ws.send(Buffer.from(`echo:${raw.toString()}`), { binary: true });
        });
      },
    });
    const info = await listener.listen();
    pairNode(viewer.db, {
      id: owner.identity.nodeId,
      name: 'gpu-box',
      url: `http://127.0.0.1:${info.port}`,
      publicKey: publicKeyB64(owner.identity),
      apiKey: 'k',
    });
    pairNode(owner.db, {
      id: viewer.identity.nodeId,
      name: 'laptop',
      url: 'http://127.0.0.1',
      publicKey: publicKeyB64(viewer.identity),
      apiKey: 'k',
    });
    return info;
  }

  /** A stand-in for the local client's end of the pane socket. */
  function fakeClient() {
    const received: Array<{ text: string; binary: boolean }> = [];
    const handlers: any = {};
    const ws: any = {
      readyState: 1,
      send: (data: any, opts: any) =>
        received.push({ text: Buffer.from(data).toString(), binary: Boolean(opts && opts.binary) }),
      close: () => { ws.readyState = 3; },
      on: (event: string, fn: any) => { handlers[event] = fn; },
      emit: (event: string, ...args: any[]) => handlers[event] && handlers[event](...args),
    };
    return { ws, received };
  }

  it('carries the replay and live output back to the client', async () => {
    await startOwner();
    const { ws, received } = fakeClient();

    const peer = await relayPane({
      clientWs: ws,
      nodeId: owner.identity.nodeId,
      paneId: 'p1',
      identity: viewer.identity,
      db: viewer.db,
    });
    sockets.push(peer);
    await new Promise((r) => setTimeout(r, 200));

    expect(received.map((f) => f.text)).toEqual([
      '{"type":"replay-start"}',
      'screen of p1',
      '{"type":"replay-end"}',
    ]);
    // The screen restore must stay binary; sent as text it would be parsed as
    // a JSON frame by the client and dropped.
    expect(received[1].binary).toBe(true);
    expect(received[0].binary).toBe(false);
  });

  it('carries keystrokes to the machine that owns the pane', async () => {
    await startOwner();
    const { ws, received } = fakeClient();
    const peer = await relayPane({
      clientWs: ws,
      nodeId: owner.identity.nodeId,
      paneId: 'p1',
      identity: viewer.identity,
      db: viewer.db,
    });
    sockets.push(peer);
    await new Promise((r) => setTimeout(r, 150));

    ws.emit('message', Buffer.from('{"t":"i","data":"ls\\r"}'), false);
    await new Promise((r) => setTimeout(r, 200));

    expect(received.some((f) => f.text.includes('echo:{"t":"i","data":"ls\\r"}'))).toBe(true);
  });

  it('tells the client when the machine is not paired', async () => {
    const { ws, received } = fakeClient();
    const result = await relayPane({
      clientWs: ws,
      nodeId: 'never-paired',
      paneId: 'p1',
      identity: viewer.identity,
      db: viewer.db,
    });
    expect(result).toBeNull();
    expect(received[0].text).toContain('not_found');
    expect(ws.readyState).toBe(3);
  });

  it('tells the client when the machine cannot be reached', async () => {
    pairNode(viewer.db, {
      id: 'dead',
      name: 'dead-box',
      url: 'http://127.0.0.1:9',
      publicKey: 'AAAA',
      apiKey: 'k',
    });
    const { ws, received } = fakeClient();
    const result = await relayPane({
      clientWs: ws,
      nodeId: 'dead',
      paneId: 'p1',
      identity: viewer.identity,
      db: viewer.db,
    });
    expect(result).toBeNull();
    expect(received[0].text).toContain('dead-box');
  });

  it('closes the client socket when the far end goes away', async () => {
    // Otherwise the client watches a pane that stopped existing, with no
    // frame to say so and no reason to start its reattach ladder.
    await startOwner();
    const { ws } = fakeClient();
    const peer = await relayPane({
      clientWs: ws,
      nodeId: owner.identity.nodeId,
      paneId: 'p1',
      identity: viewer.identity,
      db: viewer.db,
    });
    await new Promise((r) => setTimeout(r, 150));
    peer.close();
    await new Promise((r) => setTimeout(r, 200));
    expect(ws.readyState).toBe(3);
  });
});
