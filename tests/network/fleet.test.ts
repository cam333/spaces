import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createFleet, toNetNode, KEY_HINT_CHARS } from '../../bin/lib/network/fleet.js';
import { openNodeDb, pairNode } from '../../bin/lib/network/nodes.js';

let dbPath: string;

beforeEach(() => {
  dbPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-fleet-')), 'network.db');
});

describe('fleet view', () => {
  it('always reports this machine first, even with no peers', () => {
    const fleet = createFleet({ nodeId: 'me', dbPath });
    const nodes = fleet.listNodes();
    expect(nodes).toHaveLength(1);
    expect(nodes[0].self).toBe(true);
    expect(nodes[0].nodeId).toBe('me');
    expect(nodes[0].status).toBe('online');
  });

  it('lists paired peers after this machine', () => {
    const fleet = createFleet({ nodeId: 'me', dbPath });
    fleet.pair({ url: 'https://gpu:3479', publicKey: 'PUBKEYPUBKEYPUBKEY', name: 'gpu-box', nodeId: 'gpu' });

    const nodes = fleet.listNodes();
    expect(nodes.map((n: any) => n.nodeId)).toEqual(['me', 'gpu']);
    expect(nodes[1].paired).toBe(true);
    expect(nodes[1].name).toBe('gpu-box');
  });

  it('never sends key material to a client', () => {
    // The single rule this module exists to enforce.
    const fleet = createFleet({ nodeId: 'me', dbPath });
    fleet.pair({
      url: 'https://gpu',
      publicKey: 'PUBKEYPUBKEYPUBKEYPUBKEY',
      apiKey: 'spk_live_secret',
      nodeId: 'gpu',
    });

    const serialised = JSON.stringify(fleet.listNodes());
    expect(serialised).not.toContain('spk_live_secret');
    expect(serialised).not.toContain('PUBKEYPUBKEYPUBKEYPUBKEY');
    // …but enough of the key to tell two apart in a list.
    const peer = fleet.listNodes()[1];
    expect(peer.keyHint).toBe('PUBKEYPUBKEY'.slice(0, KEY_HINT_CHARS));
  });

  it('reports a discovered-but-unpaired machine as unpaired', () => {
    const db = openNodeDb({ path: dbPath });
    db.prepare(
      `INSERT INTO nodes (id, name, url, api_key_encrypted, discovered_via)
       VALUES ('seen', 'studio-pc', 'http://studio-pc', '', 'mdns')`
    ).run();

    const peer = createFleet({ nodeId: 'me', dbPath }).listNodes()[1];
    expect(peer.paired).toBe(false);
    expect(peer.status).toBe('unpaired');
    expect(peer.keyHint).toBeNull();
  });

  it('never lists this machine twice', () => {
    // A peer table that somehow contains our own id must not produce a
    // duplicate row in the sidebar.
    const db = openNodeDb({ path: dbPath });
    pairNode(db, { id: 'me', name: 'me', url: 'https://me', publicKey: 'K', apiKey: 'k' });

    const nodes = createFleet({ nodeId: 'me', dbPath }).listNodes();
    expect(nodes.filter((n: any) => n.nodeId === 'me')).toHaveLength(1);
  });

  it('treats an unusable peer database as an empty fleet', () => {
    // Federation is opt-in. A machine that never paired must still serve its
    // own panes, so a missing or broken network.db is not an error.
    const fleet = createFleet({ nodeId: 'me', dbPath: path.join(os.tmpdir(), 'no', 'such', 'x.db') });
    expect(() => fleet.listNodes()).not.toThrow();
    expect(fleet.listNodes()[0].self).toBe(true);
  });

  it('emits an event when a machine is paired or forgotten', () => {
    const seen: any[] = [];
    const fleet = createFleet({ nodeId: 'me', dbPath, emit: (t, p) => seen.push({ t, p }) });
    fleet.pair({ url: 'https://gpu', publicKey: 'K', nodeId: 'gpu' });
    fleet.forget('gpu');
    expect(seen.map((s) => s.t)).toEqual(['net.node.updated', 'net.node.updated']);
    expect(seen[1].p.removed).toBe(true);
  });

  it('forgetting a machine it does not know is not an error', () => {
    const fleet = createFleet({ nodeId: 'me', dbPath });
    expect(fleet.forget('never-paired')).toBe(false);
  });

  it('derives a stable id when pairing without one', () => {
    const fleet = createFleet({ nodeId: 'me', dbPath });
    const first = fleet.pair({ url: 'https://x', publicKey: 'SAMEKEY' });
    const again = fleet.pair({ url: 'https://x', publicKey: 'SAMEKEY' });
    expect(first.nodeId).toBe(again.nodeId);
    expect(fleet.listNodes()).toHaveLength(2); // self + one peer, not two
  });
});

describe('wire shape', () => {
  it('marks a row as this machine only when told to', () => {
    const row = { id: 'x', name: 'x', url: '', public_key: 'K' };
    expect(toNetNode(row).self).toBe(false);
    expect(toNetNode(row, { self: true }).self).toBe(true);
  });
});

describe('live reachability', () => {
  it('prefers what the link manager knows over what the table last recorded', () => {
    // The `status` column says what was last persisted; the link manager knows
    // whether this machine is talking to the peer right now. Reporting the
    // column left a peer we were actively driving reading `unknown`, which a
    // client renders as forever connecting.
    const db = openNodeDb({ path: dbPath });
    pairNode(db, { id: 'gpu', name: 'gpu-box', url: 'https://gpu', publicKey: 'K', apiKey: 'k' });

    const stale = createFleet({ nodeId: 'me', dbPath }).listNodes()[1];
    expect(stale.status).toBe('unknown');

    const live = createFleet({ nodeId: 'me', dbPath, statusOf: () => 'online' }).listNodes()[1];
    expect(live.status).toBe('online');
  });

  it('falls back to the table when the link manager has nothing to say', () => {
    const db = openNodeDb({ path: dbPath });
    pairNode(db, { id: 'gpu', name: 'gpu-box', url: 'https://gpu', publicKey: 'K', apiKey: 'k' });
    const fleet = createFleet({ nodeId: 'me', dbPath, statusOf: () => null });
    expect(fleet.listNodes()[1].status).toBe('unknown');
  });

  it('never lets a peer status override this machine being online', () => {
    const fleet = createFleet({ nodeId: 'me', dbPath, statusOf: () => 'offline' });
    expect(fleet.listNodes()[0].status).toBe('online');
  });
});
