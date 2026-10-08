/**
 * Mutual challenge–response between two federated nodes.
 *
 * The property this exists to provide: authenticating to a peer must not
 * involve sending it anything reusable. A bearer token — the mechanism the
 * legacy `/ws` federation path uses, passed in a query string — is replayable
 * by anyone who captures it, and lands in access logs on the way. A signature
 * over a nonce the *verifier* chose is worth nothing to anyone who intercepts
 * it.
 *
 * # What the signature covers, and why each part is there
 *
 *   "spaces-federation-v1" \0 purpose \0 callerNodeId \0 peerNodeId \0 nonce
 *
 * - **Protocol tag** — domain separation. A signature produced here can never
 *   be valid in some later protocol that signs with the same key.
 * - **Purpose** — a proof of "I may federate" must not double as a proof of
 *   anything else that comes later.
 * - **Caller id** — binds the proof to who is claiming it.
 * - **Peer id** — the one most easily overlooked. Without it, a peer you
 *   connect to can take the proof you addressed to *it* and present it to a
 *   third node as though you had authenticated there. Pinned by
 *   `will not let a proof for one peer be replayed at another`.
 * - **Nonce** — freshness, chosen by the verifier so the prover cannot
 *   precompute.
 *
 * NUL separators rather than concatenation: without them, ("ab","c") and
 * ("a","bc") produce the same bytes, and a node id is attacker-influenced in
 * exactly the way that matters.
 *
 * # What this does NOT establish
 *
 * That the *transport* is authentic. A handshake proves the far end holds a
 * key; it says nothing about whether something sits in the middle relaying.
 * That is what certificate pinning is for, and it belongs with the listener.
 */

const crypto = require('crypto');

/** Domain-separation tag. Bump if the signed layout ever changes. */
const PROTOCOL_TAG = 'spaces-federation-v1';

/** The only purpose defined so far: opening a federated control channel. */
const PURPOSE_FEDERATION = 'federation-connect';

/** Nonce size. 32 bytes is well past any birthday concern for a live window. */
const NONCE_BYTES = 32;

/**
 * How long a challenge stays answerable. Long enough for a round trip on a
 * bad link, short enough that a captured nonce is worthless by the time it
 * could be used — and it is single-use regardless.
 */
const DEFAULT_TTL_MS = 30_000;

function createChallenge({ ttlMs = DEFAULT_TTL_MS, now = Date.now } = {}) {
  return {
    nonce: crypto.randomBytes(NONCE_BYTES).toString('base64'),
    expiresAt: now() + ttlMs,
  };
}

/**
 * The exact bytes both sides sign and verify. Single source of truth — a
 * prover and a verifier that build this differently fail closed, but they fail
 * mysteriously, so neither side is allowed its own copy.
 */
function challengeMessage({ nonce, callerNodeId, peerNodeId, purpose }) {
  return Buffer.from(
    [PROTOCOL_TAG, purpose, callerNodeId, peerNodeId, nonce].join('\0'),
    'utf-8'
  );
}

/** Signs a challenge as `identity`. Returns base64. */
function proveChallenge(identity, { nonce, callerNodeId, peerNodeId, purpose }) {
  const { sign } = require('./identity');
  return sign(identity, challengeMessage({ nonce, callerNodeId, peerNodeId, purpose }));
}

/**
 * Verifies a proof against the public key **recorded at pair time** — never
 * one offered during the exchange, which is the caller's unverified claim
 * about itself.
 */
function verifyProof({ publicKeyB64, signatureB64, nonce, callerNodeId, peerNodeId, purpose }) {
  const { verify } = require('./identity');
  if (!publicKeyB64 || !signatureB64 || !nonce || !callerNodeId || !peerNodeId || !purpose) {
    return false;
  }
  return verify(
    publicKeyB64,
    challengeMessage({ nonce, callerNodeId, peerNodeId, purpose }),
    signatureB64
  );
}

/**
 * Tracks outstanding challenges so each is answerable exactly once.
 *
 * The signature check alone does not stop replay: a proof captured on the wire
 * stays valid for its nonce forever unless the verifier remembers having used
 * it. This is that memory.
 *
 * Expired entries are swept on issue rather than by a timer — a challenge is
 * created per connection attempt, so an unauthenticated caller could otherwise
 * pin memory just by connecting and never answering.
 */
function createNonceStore({ now = Date.now, ttlMs = DEFAULT_TTL_MS } = {}) {
  const outstanding = new Map();

  function sweep() {
    const t = now();
    for (const [nonce, expiresAt] of outstanding) {
      if (expiresAt <= t) outstanding.delete(nonce);
    }
  }

  return {
    issue() {
      sweep();
      const challenge = createChallenge({ ttlMs, now });
      outstanding.set(challenge.nonce, challenge.expiresAt);
      return challenge;
    },
    /** `true` exactly once per issued, unexpired nonce. */
    consume(nonce) {
      const expiresAt = outstanding.get(nonce);
      if (expiresAt === undefined) return false;
      outstanding.delete(nonce);
      return expiresAt > now();
    },
    size() {
      return outstanding.size;
    },
  };
}

module.exports = {
  createChallenge,
  challengeMessage,
  proveChallenge,
  verifyProof,
  createNonceStore,
  PROTOCOL_TAG,
  PURPOSE_FEDERATION,
  DEFAULT_TTL_MS,
};
