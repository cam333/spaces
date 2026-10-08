import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  loadOrCreateIdentity,
  publicKeyB64,
  sign,
  verify,
} from '../../bin/lib/network/identity.js';

let dir: string;

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-identity-'));
});

describe('node identity', () => {
  it('creates a keypair once and reuses it', () => {
    const first = loadOrCreateIdentity({ dir });
    const second = loadOrCreateIdentity({ dir });
    expect(first.nodeId).toBe(second.nodeId);
    expect(publicKeyB64(first)).toBe(publicKeyB64(second));
    expect(publicKeyB64(first)).toMatch(/^[A-Za-z0-9+/]+=*$/);
  });

  it('keeps the private key out of node.json', () => {
    // node.json is read, copied and printed by several call sites. A private
    // key in it would leak through any one of them.
    loadOrCreateIdentity({ dir });
    const raw = fs.readFileSync(path.join(dir, 'node.json'), 'utf-8');
    expect(raw).not.toMatch(/PRIVATE KEY/);
    const parsed = JSON.parse(raw);
    expect(parsed.publicKey).toBeTruthy();
    expect(parsed.privateKey).toBeUndefined();
    expect(fs.existsSync(path.join(dir, 'node_key'))).toBe(true);
  });

  it('preserves an existing nodeId when adding a key to an older install', () => {
    // Upgrade path: node.json already exists from before keys were a thing.
    fs.writeFileSync(
      path.join(dir, 'node.json'),
      JSON.stringify({ nodeId: 'legacy-uuid', nodeName: 'old-box', createdAt: 'then' })
    );
    const identity = loadOrCreateIdentity({ dir });
    expect(identity.nodeId).toBe('legacy-uuid');
    expect(identity.nodeName).toBe('old-box');
    expect(publicKeyB64(identity)).toBeTruthy();
  });

  it('restricts the private key file to its owner', function () {
    const identity = loadOrCreateIdentity({ dir });
    expect(identity.privateKeyPem).toMatch(/PRIVATE KEY/);
    if (process.platform === 'win32') return; // POSIX bits are meaningless here
    const mode = fs.statSync(path.join(dir, 'node_key')).mode & 0o777;
    expect(mode).toBe(0o600);
  });

  it('verifies a signature under the right key and rejects a wrong one', () => {
    const mine = loadOrCreateIdentity({ dir });
    const theirs = loadOrCreateIdentity({
      dir: fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-identity-')),
    });
    const message = Buffer.from('attach to pane p1');
    const signature = sign(mine, message);

    expect(verify(publicKeyB64(mine), message, signature)).toBe(true);
    expect(verify(publicKeyB64(theirs), message, signature)).toBe(false);
  });

  it('rejects a tampered message', () => {
    const identity = loadOrCreateIdentity({ dir });
    const signature = sign(identity, Buffer.from('transfer 1'));
    expect(verify(publicKeyB64(identity), Buffer.from('transfer 2'), signature)).toBe(false);
  });

  it('rejects malformed keys and signatures instead of throwing', () => {
    // Every one of these arrives from the network in a later phase.
    const identity = loadOrCreateIdentity({ dir });
    const message = Buffer.from('hello');
    const good = sign(identity, message);
    expect(verify('not-base64!!', message, good)).toBe(false);
    expect(verify(publicKeyB64(identity), message, 'not-base64!!')).toBe(false);
    expect(verify('', message, good)).toBe(false);
    expect(verify(publicKeyB64(identity), message, '')).toBe(false);
  });
});
