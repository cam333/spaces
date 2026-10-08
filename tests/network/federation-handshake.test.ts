import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadOrCreateIdentity, publicKeyB64 } from '../../bin/lib/network/identity.js';
import {
  proveChallenge,
  verifyProof,
  createNonceStore,
  PURPOSE_FEDERATION,
} from '../../bin/lib/network/handshake.js';
import { openNodeDb, pairNode, getNode } from '../../bin/lib/network/nodes.js';

/**
 * A node: its own identity plus its own view of who it trusts. Two of these,
 * in separate directories with separate databases, are as close to two
 * machines as a single process gets.
 */
function makeNode(label: string) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `spaces-node-${label}-`));
  const identity = loadOrCreateIdentity({ dir });
  const db = openNodeDb({ path: path.join(dir, 'network.db') });
  return { identity, db, nonces: createNonceStore() };
}

type Node = ReturnType<typeof makeNode>;

/** What pairing does: each side records the other's public key. */
function pairMutually(a: Node, b: Node) {
  pairNode(a.db, {
    id: b.identity.nodeId,
    name: b.identity.nodeName,
    url: 'https://b',
    publicKey: publicKeyB64(b.identity),
    apiKey: 'k',
  });
  pairNode(b.db, {
    id: a.identity.nodeId,
    name: a.identity.nodeName,
    url: 'https://a',
    publicKey: publicKeyB64(a.identity),
    apiKey: 'k',
  });
}

/**
 * One direction of the handshake, exactly as a listener would run it:
 * the verifier issues a nonce, the caller signs it, and the verifier checks
 * the signature against **the key it recorded at pair time** — never the one
 * the caller presents about itself.
 */
function authenticate(caller: Node, verifier: Node, opts: { presentedKey?: string } = {}) {
  const challenge = verifier.nonces.issue();
  const ctx = {
    nonce: challenge.nonce,
    callerNodeId: caller.identity.nodeId,
    peerNodeId: verifier.identity.nodeId,
    purpose: PURPOSE_FEDERATION,
  };
  const signature = proveChallenge(caller.identity, ctx);

  // Verifier side.
  if (!verifier.nonces.consume(ctx.nonce)) return false;
  const known = getNode(verifier.db, ctx.callerNodeId);
  if (!known || !known.public_key) return false;
  // `presentedKey` models a caller that lies about its own key in the greeting.
  // It is accepted as an argument and then deliberately ignored, which is the
  // property under test.
  void opts.presentedKey;
  return verifyProof({ ...ctx, publicKeyB64: known.public_key, signatureB64: signature });
}

let alice: Node;
let bob: Node;
let mallory: Node;

beforeEach(() => {
  alice = makeNode('alice');
  bob = makeNode('bob');
  mallory = makeNode('mallory');
});

describe('two paired nodes', () => {
  it('authenticate each other in both directions', () => {
    pairMutually(alice, bob);
    expect(authenticate(alice, bob)).toBe(true);
    expect(authenticate(bob, alice)).toBe(true);
  });

  it('reject a node nobody paired with', () => {
    pairMutually(alice, bob);
    expect(authenticate(mallory, bob)).toBe(false);
  });

  it('reject a caller that presents an attacker key in its greeting', () => {
    // The greeting is the caller's unverified claim about itself. Trusting it
    // would make the whole exchange decorative: anyone could hand over a key
    // they hold and sign with it.
    pairMutually(alice, bob);
    const impostor = { ...mallory, identity: { ...mallory.identity, nodeId: alice.identity.nodeId } };
    expect(
      authenticate(impostor as Node, bob, { presentedKey: publicKeyB64(mallory.identity) })
    ).toBe(false);
  });

  it('reject a replayed nonce', () => {
    pairMutually(alice, bob);
    const challenge = bob.nonces.issue();
    const ctx = {
      nonce: challenge.nonce,
      callerNodeId: alice.identity.nodeId,
      peerNodeId: bob.identity.nodeId,
      purpose: PURPOSE_FEDERATION,
    };
    const signature = proveChallenge(alice.identity, ctx);
    const known = getNode(bob.db, alice.identity.nodeId);

    const attempt = () =>
      bob.nonces.consume(ctx.nonce) &&
      verifyProof({ ...ctx, publicKeyB64: known.public_key, signatureB64: signature });

    expect(attempt()).toBe(true);
    expect(attempt()).toBe(false); // the same proof, captured and resent
  });

  it('reject a proof captured on one node and presented to another', () => {
    // alice is paired with both bob and carol. A proof alice made for bob must
    // be worthless to bob if bob turns around and presents it to carol.
    const carol = makeNode('carol');
    pairMutually(alice, bob);
    pairMutually(alice, carol);

    const forBob = {
      nonce: bob.nonces.issue().nonce,
      callerNodeId: alice.identity.nodeId,
      peerNodeId: bob.identity.nodeId,
      purpose: PURPOSE_FEDERATION,
    };
    const signature = proveChallenge(alice.identity, forBob);

    const atCarol = { ...forBob, peerNodeId: carol.identity.nodeId };
    const aliceAtCarol = getNode(carol.db, alice.identity.nodeId);
    expect(
      verifyProof({ ...atCarol, publicKeyB64: aliceAtCarol.public_key, signatureB64: signature })
    ).toBe(false);
  });

  it('stop trusting a node that was forgotten', () => {
    pairMutually(alice, bob);
    expect(authenticate(alice, bob)).toBe(true);
    bob.db.prepare('DELETE FROM nodes WHERE id = ?').run(alice.identity.nodeId);
    expect(authenticate(alice, bob)).toBe(false);
  });

  it('stop trusting the old key after the peer rotates', () => {
    pairMutually(alice, bob);
    const staleSignature = proveChallenge(alice.identity, {
      nonce: bob.nonces.issue().nonce,
      callerNodeId: alice.identity.nodeId,
      peerNodeId: bob.identity.nodeId,
      purpose: PURPOSE_FEDERATION,
    });

    // alice regenerates; bob re-pairs against the new key.
    const rotatedDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-node-alice2-'));
    const rotated = loadOrCreateIdentity({ dir: rotatedDir });
    pairNode(bob.db, {
      id: alice.identity.nodeId,
      name: 'alice',
      url: 'https://a',
      publicKey: publicKeyB64(rotated),
      apiKey: 'k',
    });

    const known = getNode(bob.db, alice.identity.nodeId);
    expect(
      verifyProof({
        nonce: 'irrelevant',
        callerNodeId: alice.identity.nodeId,
        peerNodeId: bob.identity.nodeId,
        purpose: PURPOSE_FEDERATION,
        publicKeyB64: known.public_key,
        signatureB64: staleSignature,
      })
    ).toBe(false);
  });
});
