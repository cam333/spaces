import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadOrCreateIdentity, publicKeyB64 } from '../../bin/lib/network/identity.js';
import { openNodeDb, pairNode } from '../../bin/lib/network/nodes.js';
import { createFederationListener } from '../../bin/lib/network/listener.js';
import { connectToPeer, FederationError } from '../../bin/lib/network/client.js';
import { createAuditLog } from '../../bin/lib/network/audit.js';

function machine(label: string) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `spaces-m-${label}-`));
  return {
    dir,
    identity: loadOrCreateIdentity({ dir }),
    db: openNodeDb({ path: path.join(dir, 'network.db') }),
    auditPath: path.join(dir, 'federation.log'),
  };
}
type Machine = ReturnType<typeof machine>;

function pair(from: Machine, to: Machine, url = 'https://x') {
  pairNode(from.db, {
    id: to.identity.nodeId,
    name: to.identity.nodeName,
    url,
    publicKey: publicKeyB64(to.identity),
    apiKey: 'k',
  });
}

let server: Machine;
let caller: Machine;
let stranger: Machine;
let listener: any;
let sockets: any[] = [];

beforeEach(() => {
  sockets = [];
  server = machine('server');
  caller = machine('caller');
  stranger = machine('stranger');
});

afterEach(async () => {
  for (const s of sockets) { try { s.terminate(); } catch { /* gone */ } }
  if (listener) await listener.close();
  listener = null;
});

async function startListener(overrides: Record<string, unknown> = {}) {
  listener = createFederationListener({
    iface: '127.0.0.1',
    port: 0,
    identity: server.identity,
    db: server.db,
    audit: createAuditLog({ path: server.auditPath }),
    ...overrides,
  });
  const info = await listener.listen();
  return `ws://127.0.0.1:${info.port}/federation`;
}

describe('federation listener', () => {
  it('refuses to start on a public interface', () => {
    // The rail must hold before a socket is ever opened.
    expect(() =>
      createFederationListener({
        iface: '8.8.8.8',
        identity: server.identity,
        db: server.db,
      })
    ).toThrow(/public/i);
  });

  it('refuses the wildcard even with public binding allowed', () => {
    expect(() =>
      createFederationListener({
        iface: '0.0.0.0',
        allowPublic: true,
        identity: server.identity,
        db: server.db,
      })
    ).toThrow(/every interface at once/i);
  });

  it('completes a mutual handshake between paired machines', async () => {
    pair(server, caller);
    pair(caller, server);
    const url = await startListener();

    const ws = await connectToPeer({
      url,
      peerNodeId: server.identity.nodeId,
      identity: caller.identity,
      db: caller.db,
    });
    expect(ws.readyState).toBe(1);
    ws.close();
  });

  it('refuses a machine nobody paired with', async () => {
    pair(stranger, server); // the stranger knows us; we do not know it
    const url = await startListener();

    await expect(
      connectToPeer({
        url,
        peerNodeId: server.identity.nodeId,
        identity: stranger.identity,
        db: stranger.db,
      })
    ).rejects.toThrow(FederationError);
  });

  it('refuses a caller holding the wrong private key for a paired id', async () => {
    // The impostor claims a paired node's id but signs with its own key. This
    // is the attack the stored-key rule exists to stop.
    pair(server, caller);
    pair(stranger, server);
    const url = await startListener();

    const impostor = { ...stranger.identity, nodeId: caller.identity.nodeId };
    await expect(
      connectToPeer({
        url,
        peerNodeId: server.identity.nodeId,
        identity: impostor,
        db: stranger.db,
      })
    ).rejects.toThrow(FederationError);
  });

  it('will not prove itself to a listener it cannot verify', async () => {
    // A caller paired with the *wrong* key for this listener must hang up
    // before signing, so an impostor never harvests a usable proof.
    pair(server, caller);
    pairNode(caller.db, {
      id: server.identity.nodeId,
      name: 'server',
      url: 'https://x',
      publicKey: publicKeyB64(stranger.identity), // wrong key on purpose
      apiKey: 'k',
    });
    const url = await startListener();

    await expect(
      connectToPeer({
        url,
        peerNodeId: server.identity.nodeId,
        identity: caller.identity,
        db: caller.db,
      })
    ).rejects.toMatchObject({ code: 'peer_unverified' });
  });

  it('refuses to connect to a peer it has no record of', async () => {
    const url = await startListener();
    await expect(
      connectToPeer({
        url,
        peerNodeId: 'never-heard-of-it',
        identity: caller.identity,
        db: caller.db,
      })
    ).rejects.toMatchObject({ code: 'not_paired' });
  });

  it('serves nothing but the federation path', async () => {
    pair(server, caller);
    pair(caller, server);
    const url = await startListener();
    const wrongPath = url.replace('/federation', '/proto');

    await expect(
      connectToPeer({
        url: wrongPath,
        peerNodeId: server.identity.nodeId,
        identity: caller.identity,
        db: caller.db,
      })
    ).rejects.toThrow(FederationError);
  });

  it('records accepted and refused attempts in the audit log', async () => {
    pair(server, caller);
    pair(caller, server);
    const url = await startListener();

    await connectToPeer({
      url,
      peerNodeId: server.identity.nodeId,
      identity: caller.identity,
      db: caller.db,
    }).then((ws) => ws.close());

    await connectToPeer({
      url,
      peerNodeId: server.identity.nodeId,
      identity: stranger.identity,
      db: (() => {
        pair(stranger, server);
        return stranger.db;
      })(),
    }).catch(() => undefined);

    // Give the listener a moment to finish writing the refusal.
    await new Promise((r) => setTimeout(r, 100));
    const lines = fs
      .readFileSync(server.auditPath, 'utf-8')
      .trim()
      .split('\n')
      .map((l) => JSON.parse(l));
    expect(lines.some((l) => l.event === 'listener.started')).toBe(true);
    expect(lines.some((l) => l.event === 'connect.accepted')).toBe(true);
    expect(lines.some((l) => l.event === 'connect.rejected')).toBe(true);
    // No signature, nonce or key material anywhere in it.
    const raw = fs.readFileSync(server.auditPath, 'utf-8');
    expect(raw).not.toMatch(/"signature"/);
    expect(raw).not.toMatch(/"nonce"/);
  });

  it('hands the authenticated socket to its caller', async () => {
    pair(server, caller);
    pair(caller, server);
    let authenticatedAs: string | null = null;
    const url = await startListener({
      onPeer: (ws: any, node: any) => {
        authenticatedAs = node.id;
        ws.send(JSON.stringify({ type: 'hello-from-runtime' }));
      },
    });

    // onMessage is registered before the exchange begins, so nothing the peer
    // sends immediately after `welcome` can be dropped.
    let first: string | null = null;
    const arrived = new Promise<void>((resolve) => {
      void connectToPeer({
        url,
        peerNodeId: server.identity.nodeId,
        identity: caller.identity,
        db: caller.db,
        onMessage: (raw: any) => {
          first = raw.toString();
          resolve();
        },
      }).then((ws) => sockets.push(ws));
    });
    await arrived;
    expect(JSON.parse(first!).type).toBe('hello-from-runtime');
    expect(authenticatedAs).toBe(caller.identity.nodeId);
  });
});
