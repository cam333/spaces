import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadOrCreateIdentity, publicKeyB64 } from '../../bin/lib/network/identity.js';
import { openNodeDb, pairNode } from '../../bin/lib/network/nodes.js';
import { createFederationListener } from '../../bin/lib/network/listener.js';
import { createPeerLinks, federationUrl } from '../../bin/lib/network/links.js';
import { createAuditLog } from '../../bin/lib/network/audit.js';

describe('federation url', () => {
  it('rewrites a machine web address to its federation socket', () => {
    // The federation port is separate from the web port by design, so the
    // stored URL's scheme and path are rewritten and its port replaced.
    expect(federationUrl('https://gpu-box:3457')).toBe('wss://gpu-box:3457/federation');
    expect(federationUrl('http://gpu-box')).toBe('ws://gpu-box:3479/federation');
    expect(federationUrl('http://10.0.0.5:9000')).toBe('ws://10.0.0.5:9000/federation');
  });

  it('normalises a websocket url onto the federation path and port', () => {
    expect(federationUrl('ws://gpu-box/anything')).toBe('ws://gpu-box:3479/federation');
  });

  it('passes through something it cannot parse rather than mangling it', () => {
    // A hand-built value from a config file should reach the socket unchanged
    // and fail there with a clear error, not be silently rewritten into
    // something that connects to the wrong place.
    expect(federationUrl('gpu-box:3479')).toBe('gpu-box:3479');
    expect(federationUrl('')).toBe('');
  });
});

function machine(label: string) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `spaces-l-${label}-`));
  return {
    dir,
    identity: loadOrCreateIdentity({ dir }),
    db: openNodeDb({ path: path.join(dir, 'network.db') }),
  };
}

let server: any;
let caller: any;
let listener: any;
let links: any;

beforeEach(() => {
  server = machine('server');
  caller = machine('caller');
});

afterEach(async () => {
  if (links) links.close();
  if (listener) await listener.close();
  links = null;
  listener = null;
});

describe('peer links', () => {
  async function start(onPeer?: any) {
    listener = createFederationListener({
      iface: '127.0.0.1',
      port: 0,
      identity: server.identity,
      db: server.db,
      audit: createAuditLog({ path: path.join(server.dir, 'audit.log') }),
      onPeer,
    });
    const info = await listener.listen();
    // Both directions: the handshake is mutual and one-sided pairing fails.
    pairNode(caller.db, {
      id: server.identity.nodeId,
      name: 'server',
      url: `http://127.0.0.1:${info.port}`,
      publicKey: publicKeyB64(server.identity),
      apiKey: 'k',
    });
    pairNode(server.db, {
      id: caller.identity.nodeId,
      name: 'caller',
      url: 'http://127.0.0.1',
      publicKey: publicKeyB64(caller.identity),
      apiKey: 'k',
    });
    return info;
  }

  it('carries a request to a peer and brings the answer back', async () => {
    await start((ws: any) => {
      // Stand-in for the control channel: echo the method back.
      ws.on('message', (raw: any) => {
        const m = JSON.parse(raw.toString());
        if (m.type === 'hello') return;
        ws.send(JSON.stringify({ id: m.id, result: { saw: m.method } }));
      });
    });

    links = createPeerLinks({ identity: caller.identity, db: caller.db });
    const result = await links.request(server.identity.nodeId, 'session.snapshot', {});
    expect(result).toEqual({ saw: 'session.snapshot' });
    expect(links.isLinked(server.identity.nodeId)).toBe(true);
  });

  it('surfaces a peer error as a rejection rather than a silent null', async () => {
    await start((ws: any) => {
      ws.on('message', (raw: any) => {
        const m = JSON.parse(raw.toString());
        if (m.type === 'hello') return;
        ws.send(JSON.stringify({ id: m.id, error: { code: 'unauthorized', message: 'nope' } }));
      });
    });

    links = createPeerLinks({ identity: caller.identity, db: caller.db });
    await expect(
      links.request(server.identity.nodeId, 'pane.create', {})
    ).rejects.toThrow(/unauthorized: nope/);
  });

  it('forwards a peer event tagged with the machine it came from', async () => {
    await start((ws: any) => {
      ws.on('message', (raw: any) => {
        const m = JSON.parse(raw.toString());
        if (m.type === 'hello') return;
        ws.send(JSON.stringify({ id: m.id, result: { gap: false, events: [] } }));
        ws.send(JSON.stringify({ event: { seq: 4, type: 'pane.state', payload: {}, ts: 0 } }));
      });
    });

    const seen: any[] = [];
    links = createPeerLinks({
      identity: caller.identity,
      db: caller.db,
      onEvent: (nodeId: string, event: any) => seen.push({ nodeId, event }),
    });
    await links.follow(server.identity.nodeId);
    await new Promise((r) => setTimeout(r, 150));

    expect(seen).toHaveLength(1);
    expect(seen[0].nodeId).toBe(server.identity.nodeId);
    expect(seen[0].event.type).toBe('pane.state');
  });

  it('refuses a machine that was never paired', async () => {
    links = createPeerLinks({ identity: caller.identity, db: caller.db });
    await expect(links.request('unknown-machine', 'node.info', {})).rejects.toThrow(/no record/);
  });

  it('reports status transitions so the sidebar can follow along', async () => {
    await start((ws: any) => {
      ws.on('message', (raw: any) => {
        const m = JSON.parse(raw.toString());
        if (m.type !== 'hello') ws.send(JSON.stringify({ id: m.id, result: {} }));
      });
    });

    const statuses: string[] = [];
    links = createPeerLinks({
      identity: caller.identity,
      db: caller.db,
      onStatus: (_id: string, s: string) => statuses.push(s),
    });
    await links.request(server.identity.nodeId, 'node.info', {});
    expect(statuses).toEqual(['connecting', 'online']);
  });

  it('holds off after a failure instead of hammering an offline machine', async () => {
    pairNode(caller.db, {
      id: 'dead',
      name: 'dead',
      // Nothing listens here.
      url: 'http://127.0.0.1:9',
      publicKey: 'AAAA',
      apiKey: 'k',
    });
    links = createPeerLinks({ identity: caller.identity, db: caller.db, now: () => 1000 });
    await expect(links.request('dead', 'node.info', {})).rejects.toThrow();
    // Second attempt inside the cooldown fails without another dial.
    await expect(links.request('dead', 'node.info', {})).rejects.toThrow(/cooldown/);
  });

  it('fails every request in flight when the link drops', async () => {
    // A pending request whose socket dies must reject, not hang forever.
    await start((ws: any) => {
      ws.on('message', (raw: any) => {
        const m = JSON.parse(raw.toString());
        if (m.type === 'hello') return;
        ws.close(); // answer nothing
      });
    });

    links = createPeerLinks({ identity: caller.identity, db: caller.db });
    await expect(
      links.request(server.identity.nodeId, 'session.snapshot', {})
    ).rejects.toThrow(/closed/);
  });

  it('reports a clean close as idle, not as unreachable', async () => {
    // The distinction the whole status model turns on: links are lazy, so
    // having none is the resting state for a machine nobody is looking at.
    // Reporting that as offline made a healthy peer read as down, and no
    // amount of refreshing could correct it because nothing had gone wrong.
    await start((ws: any) => {
      ws.on('message', (raw: any) => {
        const m = JSON.parse(raw.toString());
        if (m.type !== 'hello') ws.send(JSON.stringify({ id: m.id, result: {} }));
      });
    });

    const statuses: string[] = [];
    links = createPeerLinks({
      identity: caller.identity,
      db: caller.db,
      onStatus: (_id: string, s: string) => statuses.push(s),
    });
    await links.request(server.identity.nodeId, 'node.info', {});
    expect(statuses).toEqual(['connecting', 'online']);

    links.close();
    await new Promise((r) => setTimeout(r, 100));
    expect(statuses[statuses.length - 1]).toBe('idle');
    expect(links.statusOf(server.identity.nodeId)).toBe('idle');
  });

  it('reports a failed dial as offline', async () => {
    pairNode(caller.db, {
      id: 'dead',
      name: 'dead',
      url: 'http://127.0.0.1:9',
      publicKey: 'AAAA',
      apiKey: 'k',
    });
    links = createPeerLinks({ identity: caller.identity, db: caller.db });
    await expect(links.request('dead', 'node.info', {})).rejects.toThrow();
    expect(links.statusOf('dead')).toBe('offline');
  });
});
