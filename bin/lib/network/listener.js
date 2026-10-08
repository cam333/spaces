/**
 * The federation listener: the only thing in Spaces that serves the runtime to
 * another machine.
 *
 * # Why this is a separate server
 *
 * It binds its own socket on its own port rather than adding a path to the main
 * HTTP server. The main server's binding is decided by desktop/multiuser mode
 * and serves the web UI, `/ws` and `/proto`; sharing it would mean one mistake
 * in that logic could expose all of them on an interface meant only for
 * federation. A separate listener can only ever serve what it is given, on the
 * address [`resolveBindAddress`] approved.
 *
 * # The exchange
 *
 *   1. C→S  hello      { nodeId, nonce }            caller names itself, challenges
 *   2. S→C  challenge  { nodeId, nonce, proof }     listener answers *and* challenges
 *   3. C→S  proof      { signature }                caller answers
 *   4. S→C  welcome    { nodeId }
 *
 * Mutual, and each nonce is chosen by the side that verifies it. Step 2 is what
 * stops an impostor listener harvesting proofs: a caller that cannot verify the
 * listener hangs up before signing anything.
 *
 * The identity a caller claims in `hello` is a claim, nothing more. Verification
 * uses the public key recorded when the two machines were paired; if there is no
 * such record the connection is refused, which is what makes pairing meaningful.
 */

const http = require('http');
const { WebSocketServer } = require('ws');

const { resolveBindAddress } = require('./binding');
const { proveChallenge, verifyProof, createNonceStore, PURPOSE_FEDERATION } = require('./handshake');
const { getNode } = require('./nodes');
const { createAuditLog } = require('./audit');

/** How long a peer has to complete the exchange before the socket is dropped. */
const HANDSHAKE_TIMEOUT_MS = 10_000;

/** Largest handshake frame accepted, in bytes. Generous for four small objects. */
const MAX_FRAME_BYTES = 8 * 1024;

/**
 * How often peers are pinged, and therefore the worst case for noticing one
 * that has gone away without saying so.
 *
 * A federated link is idle for long stretches by design — a machine nobody is
 * looking at sends nothing for hours — so silence is not evidence of death and
 * only an explicit probe distinguishes the two. Without this a half-open
 * connection (a laptop that slept, a tunnel that dropped) is never detected:
 * the socket sits ESTAB on this side forever, holding its subscriptions, while
 * the other end has long since concluded the link is gone.
 *
 * Matches the 30s sweep the local `/proto` path uses in
 * `terminal-server.js::setupProtoWss`. Note that path's sweep does *not* cover
 * these sockets: federation binds its own server, so `attachControlChannel`'s
 * `isAlive` bookkeeping was inert here until this existed.
 */
const HEARTBEAT_MS = 30_000;

/**
 * What a peer is asking for, or `null` if the path is not one we serve.
 *
 * Two shapes, both behind the same handshake:
 *   `/federation`                  the control channel
 *   `/federation/pane/<paneId>`    one pane's data channel
 *
 * A pane channel is a separate socket rather than frames multiplexed onto the
 * control channel because pane data is binary and streams continuously —
 * wrapping every PTY chunk in JSON to share one socket would cost an encode,
 * a decode and roughly a third more bytes on the hottest path there is. The
 * handshake is identical, so the second socket costs a round trip and nothing
 * conceptual.
 */
function routeOf(url) {
  const pathname = String(url).split('?')[0];
  if (pathname === '/federation') return { kind: 'control' };
  const pane = pathname.match(/^\/federation\/pane\/([^/]+)$/);
  if (pane) return { kind: 'pane', paneId: decodeURIComponent(pane[1]) };
  return null;
}

function send(ws, message) {
  if (ws.readyState === 1) ws.send(JSON.stringify(message));
}

function refuse(ws, audit, entry) {
  audit.record({ event: 'connect.rejected', ...entry });
  send(ws, { type: 'error', reason: entry.reason });
  try {
    ws.close();
  } catch {
    /* already gone */
  }
}

/**
 * Runs the listener side of the exchange on one socket.
 *
 * Resolves to the authenticated peer's node record, or `null` if the socket was
 * refused — in which case it has already been closed and the refusal audited.
 */
function authenticatePeer({ ws, identity, db, audit, nonces, address }) {
  return new Promise((resolve) => {
    let settled = false;
    let expected = 'hello';
    let peerNodeId = null;
    let serverNonce = null;

    const finish = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(result);
    };

    const timer = setTimeout(() => {
      refuse(ws, audit, {
        address,
        nodeId: peerNodeId,
        reason: 'handshake timed out',
      });
      finish(null);
    }, HANDSHAKE_TIMEOUT_MS);

    ws.on('close', () => finish(null));
    ws.on('error', () => finish(null));

    ws.on('message', (raw) => {
      if (settled) return;
      if (raw.length > MAX_FRAME_BYTES) {
        refuse(ws, audit, { address, reason: 'handshake frame too large' });
        return finish(null);
      }

      let message;
      try {
        message = JSON.parse(raw.toString());
      } catch {
        refuse(ws, audit, { address, reason: 'malformed handshake frame' });
        return finish(null);
      }

      if (message.type !== expected) {
        refuse(ws, audit, {
          address,
          nodeId: peerNodeId,
          reason: `expected "${expected}", got "${message.type}"`,
        });
        return finish(null);
      }

      if (message.type === 'hello') {
        peerNodeId = String(message.nodeId || '');
        const known = peerNodeId ? getNode(db, peerNodeId) : null;
        // Refused for the same reason whether the node is unknown or merely
        // unpaired: a caller learns only "no", never which of the two it was.
        if (!known || !known.public_key) {
          refuse(ws, audit, {
            address,
            nodeId: peerNodeId,
            reason: 'not a paired machine',
          });
          return finish(null);
        }
        if (typeof message.nonce !== 'string' || message.nonce.length < 16) {
          refuse(ws, audit, { address, nodeId: peerNodeId, reason: 'missing challenge' });
          return finish(null);
        }

        // Answer the caller's challenge before asking it to sign anything, so a
        // machine impersonating this one cannot collect a usable proof.
        const ourProof = proveChallenge(identity, {
          nonce: message.nonce,
          callerNodeId: identity.nodeId,
          peerNodeId,
          purpose: PURPOSE_FEDERATION,
        });
        const challenge = nonces.issue();
        serverNonce = challenge.nonce;
        send(ws, {
          type: 'challenge',
          nodeId: identity.nodeId,
          nonce: serverNonce,
          proof: ourProof,
        });
        expected = 'proof';
        return;
      }

      if (message.type === 'proof') {
        const known = getNode(db, peerNodeId);
        if (!nonces.consume(serverNonce)) {
          refuse(ws, audit, { address, nodeId: peerNodeId, reason: 'challenge expired or reused' });
          return finish(null);
        }
        const ok =
          known &&
          known.public_key &&
          verifyProof({
            publicKeyB64: known.public_key,
            signatureB64: String(message.signature || ''),
            nonce: serverNonce,
            callerNodeId: peerNodeId,
            peerNodeId: identity.nodeId,
            purpose: PURPOSE_FEDERATION,
          });
        if (!ok) {
          refuse(ws, audit, { address, nodeId: peerNodeId, reason: 'signature did not verify' });
          return finish(null);
        }

        send(ws, { type: 'welcome', nodeId: identity.nodeId });
        audit.record({
          event: 'connect.accepted',
          address,
          nodeId: peerNodeId,
          keyId: String(known.public_key).slice(0, 12),
        });
        return finish(known);
      }
    });
  });
}

/**
 * Binds a federation listener and calls `onPeer(ws, node)` once a peer has
 * authenticated.
 *
 * Throws `BindingRefused` before opening any socket if the requested interface
 * is not one federation may serve.
 */
function createFederationListener({
  iface,
  allowPublic = false,
  port = 3479,
  identity,
  db,
  audit = createAuditLog(),
  onPeer,
  interfaces,
}) {
  const bind = resolveBindAddress({ iface, allowPublic, ...(interfaces ? { interfaces } : {}) });
  const nonces = createNonceStore();

  const server = http.createServer((_req, res) => {
    // Nothing is served over HTTP. Federation is one WebSocket path and
    // anything else asking is told nothing about what runs here.
    res.writeHead(404).end();
  });

  const wss = new WebSocketServer({ noServer: true, maxPayload: MAX_FRAME_BYTES });

  // One sweep for every socket this listener serves, control and pane alike.
  // A peer that does not answer a ping within one tick is terminated rather
  // than left holding its subscriptions — see HEARTBEAT_MS.
  const heartbeat = setInterval(() => {
    for (const peer of wss.clients) {
      if (peer.isAlive === false) {
        audit.record({ event: 'connect.dropped', reason: 'no response to ping' });
        peer.terminate();
        continue;
      }
      peer.isAlive = false;
      try {
        peer.ping();
      } catch {
        peer.terminate();
      }
    }
  }, HEARTBEAT_MS);
  // Never keep the process alive on its own account.
  if (heartbeat.unref) heartbeat.unref();

  server.on('upgrade', (req, socket, head) => {
    const target = routeOf(req.url || '');
    if (!target) {
      socket.destroy();
      return;
    }
    wss.handleUpgrade(req, socket, head, async (ws) => {
      const address = req.socket.remoteAddress || 'unknown';
      // Liveness bookkeeping starts before the handshake: a peer that opens a
      // socket and never speaks is exactly the case the sweep exists for.
      ws.isAlive = true;
      ws.on('pong', () => {
        ws.isAlive = true;
      });
      const node = await authenticatePeer({ ws, identity, db, audit, nonces, address });
      if (node && onPeer) onPeer(ws, node, target);
    });
  });

  return {
    bind,
    async listen() {
      await new Promise((resolve, reject) => {
        server.once('error', reject);
        server.listen(port, bind.address, () => {
          server.removeListener('error', reject);
          resolve();
        });
      });
      audit.record({ event: 'listener.started', address: bind.address, kind: bind.kind, port });
      return { address: bind.address, port: server.address().port, kind: bind.kind };
    },
    /**
     * Stops listening and drops every peer.
     *
     * Peer sockets are terminated rather than closed politely, because
     * `http.Server.close()` waits for open connections to end and a federated
     * connection is long-lived by design — waiting for one would hang shutdown
     * indefinitely. Reconfiguring the bound interface must never be blocked by
     * a peer that is behaving perfectly.
     */
    async close() {
      clearInterval(heartbeat);
      for (const peer of wss.clients) {
        try {
          peer.terminate();
        } catch {
          /* already gone */
        }
      }
      wss.close();
      await new Promise((resolve) => server.close(resolve));
    },
    get port() {
      const a = server.address();
      return a ? a.port : null;
    },
  };
}

module.exports = { createFederationListener, authenticatePeer, routeOf, HANDSHAKE_TIMEOUT_MS };
