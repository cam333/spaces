/**
 * The calling side of the federation exchange.
 *
 * This runs on the machine the user is sitting at. It holds the private key and
 * the pinned records, which is exactly why the TUI does not do this itself: a
 * client that dialled peers directly would need key material, and the thing
 * with the smallest attack surface is the thing that never has any.
 *
 * The listener's half of the exchange, and why it is shaped that way, is
 * documented in `./listener.js`.
 */

const WebSocket = require('ws');

const { proveChallenge, verifyProof, createChallenge, PURPOSE_FEDERATION } = require('./handshake');
const { getNode } = require('./nodes');

const CONNECT_TIMEOUT_MS = 10_000;

class FederationError extends Error {
  constructor(message, code) {
    super(message);
    this.name = 'FederationError';
    this.code = code;
  }
}

/**
 * Opens an authenticated connection to a peer, resolving to the live socket.
 *
 * Verifies the listener before proving anything about itself: an impostor that
 * cannot answer our challenge never receives a signature it could replay
 * elsewhere.
 */
function connectToPeer({
  url,
  peerNodeId,
  identity,
  db,
  timeoutMs = CONNECT_TIMEOUT_MS,
  onMessage,
}) {
  const known = getNode(db, peerNodeId);
  if (!known || !known.public_key) {
    return Promise.reject(
      new FederationError(`no paired record for node ${peerNodeId}`, 'not_paired')
    );
  }

  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url, { handshakeTimeout: timeoutMs });
    let settled = false;
    let handshakeDone = false;
    const ourChallenge = createChallenge();

    const fail = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        ws.close();
      } catch {
        /* already gone */
      }
      reject(error);
    };

    const timer = setTimeout(
      () => fail(new FederationError('handshake timed out', 'timeout')),
      timeoutMs
    );

    // Only meaningful until the handshake completes; afterwards the socket's
    // lifecycle belongs to the caller, which attaches its own handlers.
    ws.on('error', (err) => {
      if (!handshakeDone) fail(new FederationError(err.message, 'transport'));
    });
    ws.on('close', () => {
      if (!handshakeDone) fail(new FederationError('closed during handshake', 'closed'));
    });

    ws.on('open', () => {
      ws.send(
        JSON.stringify({ type: 'hello', nodeId: identity.nodeId, nonce: ourChallenge.nonce })
      );
    });

    ws.on('message', (raw, isBinary) => {
      if (handshakeDone) {
        if (onMessage) onMessage(raw, isBinary);
        return;
      }
      if (settled) return;
      let message;
      try {
        message = JSON.parse(raw.toString());
      } catch {
        return fail(new FederationError('malformed frame from peer', 'protocol'));
      }

      if (message.type === 'error') {
        return fail(new FederationError(message.reason || 'refused', 'refused'));
      }

      if (message.type === 'challenge') {
        // The peer must be who our records say before we sign anything.
        const listenerIsWhoWePaired =
          message.nodeId === peerNodeId &&
          verifyProof({
            publicKeyB64: known.public_key,
            signatureB64: String(message.proof || ''),
            nonce: ourChallenge.nonce,
            callerNodeId: peerNodeId,
            peerNodeId: identity.nodeId,
            purpose: PURPOSE_FEDERATION,
          });
        if (!listenerIsWhoWePaired) {
          return fail(
            new FederationError('peer failed to prove its identity', 'peer_unverified')
          );
        }

        const signature = proveChallenge(identity, {
          nonce: String(message.nonce || ''),
          callerNodeId: identity.nodeId,
          peerNodeId,
          purpose: PURPOSE_FEDERATION,
        });
        ws.send(JSON.stringify({ type: 'proof', signature }));
        return;
      }

      if (message.type === 'welcome') {
        settled = true;
        clearTimeout(timer);
        handshakeDone = true;
        // The handshake listener stays attached and forwards from here on,
        // rather than being torn off and replaced by the caller's. Between
        // resolving this promise and the caller attaching its own handler
        // there would otherwise be a window where an inbound frame is emitted
        // with nobody listening and silently dropped — and the peer may start
        // talking the moment it has sent `welcome`. Hence `onMessage`, which
        // is registered before the exchange even begins.
        resolve(ws);
        return;
      }

      return fail(new FederationError(`unexpected frame "${message.type}"`, 'protocol'));
    });
  });
}

module.exports = { connectToPeer, FederationError, CONNECT_TIMEOUT_MS };
