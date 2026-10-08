import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadOrCreateIdentity, publicKeyB64 } from '../../bin/lib/network/identity.js';
import {
  createChallenge,
  proveChallenge,
  verifyProof,
  createNonceStore,
  PURPOSE_FEDERATION,
} from '../../bin/lib/network/handshake.js';

function identityIn(name: string) {
  return loadOrCreateIdentity({
    dir: fs.mkdtempSync(path.join(os.tmpdir(), `spaces-hs-${name}-`)),
  });
}

let alice: ReturnType<typeof identityIn>;
let mallory: ReturnType<typeof identityIn>;

beforeEach(() => {
  alice = identityIn('alice');
  mallory = identityIn('mallory');
});

describe('challenge-response handshake', () => {
  it('accepts a proof from the node whose key was recorded', () => {
    const { nonce } = createChallenge();
    const ctx = {
      nonce,
      callerNodeId: alice.nodeId,
      peerNodeId: 'bob-node',
      purpose: PURPOSE_FEDERATION,
    };
    const signature = proveChallenge(alice, ctx);
    expect(verifyProof({ ...ctx, publicKeyB64: publicKeyB64(alice), signatureB64: signature })).toBe(true);
  });

  it('rejects a proof signed by a different node', () => {
    const { nonce } = createChallenge();
    const ctx = {
      nonce,
      callerNodeId: alice.nodeId,
      peerNodeId: 'bob-node',
      purpose: PURPOSE_FEDERATION,
    };
    const forged = proveChallenge(mallory, ctx);
    expect(verifyProof({ ...ctx, publicKeyB64: publicKeyB64(alice), signatureB64: forged })).toBe(false);
  });

  it('will not let a proof for one peer be replayed at another', () => {
    // The whole reason the signed message carries both node ids. Without it,
    // a malicious peer could take a proof addressed to itself and present it
    // to a third node as though the caller had authenticated there.
    const { nonce } = createChallenge();
    const forBob = {
      nonce,
      callerNodeId: alice.nodeId,
      peerNodeId: 'bob-node',
      purpose: PURPOSE_FEDERATION,
    };
    const signature = proveChallenge(alice, forBob);

    const replayedAtCarol = { ...forBob, peerNodeId: 'carol-node' };
    expect(
      verifyProof({ ...replayedAtCarol, publicKeyB64: publicKeyB64(alice), signatureB64: signature })
    ).toBe(false);
  });

  it('will not let a proof be reused for a different purpose', () => {
    const { nonce } = createChallenge();
    const ctx = {
      nonce,
      callerNodeId: alice.nodeId,
      peerNodeId: 'bob-node',
      purpose: PURPOSE_FEDERATION,
    };
    const signature = proveChallenge(alice, ctx);
    expect(
      verifyProof({
        ...ctx,
        purpose: 'pane-attach',
        publicKeyB64: publicKeyB64(alice),
        signatureB64: signature,
      })
    ).toBe(false);
  });

  it('rejects a proof against a different nonce', () => {
    const ctx = {
      nonce: createChallenge().nonce,
      callerNodeId: alice.nodeId,
      peerNodeId: 'bob-node',
      purpose: PURPOSE_FEDERATION,
    };
    const signature = proveChallenge(alice, ctx);
    const other = { ...ctx, nonce: createChallenge().nonce };
    expect(verifyProof({ ...other, publicKeyB64: publicKeyB64(alice), signatureB64: signature })).toBe(false);
  });

  it('issues nonces that are unpredictable and distinct', () => {
    const seen = new Set<string>();
    for (let i = 0; i < 200; i++) seen.add(createChallenge().nonce);
    expect(seen.size).toBe(200);
    // 32 bytes, base64 — short nonces are guessable and this is the only
    // freshness guarantee the handshake has.
    const { nonce } = createChallenge();
    expect(Buffer.from(nonce, 'base64').length).toBe(32);
  });
});

describe('nonce store', () => {
  it('accepts a nonce once and never again', () => {
    const store = createNonceStore();
    const { nonce } = store.issue();
    expect(store.consume(nonce)).toBe(true);
    expect(store.consume(nonce)).toBe(false);
  });

  it('rejects a nonce it never issued', () => {
    const store = createNonceStore();
    expect(store.consume(createChallenge().nonce)).toBe(false);
  });

  it('rejects a nonce that has expired', () => {
    let clock = 1_000;
    const store = createNonceStore({ now: () => clock, ttlMs: 500 });
    const { nonce } = store.issue();
    clock = 1_600;
    expect(store.consume(nonce)).toBe(false);
  });

  it('does not grow without bound as nonces expire unused', () => {
    // A challenge is issued per connection attempt, so an unauthenticated
    // caller could otherwise pin memory just by connecting repeatedly.
    let clock = 0;
    const store = createNonceStore({ now: () => clock, ttlMs: 100 });
    for (let i = 0; i < 1000; i++) {
      store.issue();
      clock += 1;
    }
    clock += 1000;
    store.issue();
    expect(store.size()).toBeLessThan(50);
  });
});
