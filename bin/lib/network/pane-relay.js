/**
 * Streams a pane that lives on another machine.
 *
 * This is the point of federation: everything else lists what exists, and this
 * is what lets you watch and drive it. A local client opens
 * `/proto/pane/<id>?nodeId=<peer>` exactly as it opens any pane, and this pipes
 * that socket to the same pane channel on the machine that owns it.
 *
 * # Verbatim, in both directions
 *
 * The pane protocol is a mix of JSON text frames (`replay-start`, `replay-end`,
 * `exit`, `error`) and binary frames (PTY output), and this relay does not
 * parse either. Bytes in, bytes out, binary flag preserved. Two consequences
 * worth stating:
 *
 *   - The client needs no idea a pane is remote. Its replay, its scrollback,
 *     its resize handling and its error codes all work unchanged, because it
 *     is talking to the same protocol it always was.
 *   - A protocol change on one side does not need a change here. Anything this
 *     relay understood would be a thing it could get wrong.
 *
 * # What it is not
 *
 * Not a cache and not a buffer. If the link to the peer drops, this socket
 * closes and the client's existing reattach ladder takes over — the same path
 * a local pane takes when its socket dies, which is already tested and already
 * backs off sensibly.
 */

const { connectToPeer } = require('./client');
const { getNode } = require('./nodes');
const { federationUrl } = require('./links');

/**
 * Pipes `clientWs` to `paneId` on `nodeId`.
 *
 * Resolves once the relay is established, or rejects having already told the
 * client why. The caller does not need to close anything on failure.
 */
async function relayPane({ clientWs, nodeId, paneId, identity, db, mode = 'observe', takeover = false }) {
  const sendError = (code, message) => {
    if (clientWs.readyState === 1) {
      clientWs.send(JSON.stringify({ type: 'error', code, message }));
    }
    try {
      clientWs.close();
    } catch {
      /* already gone */
    }
  };

  const row = getNode(db, nodeId);
  if (!row || !row.public_key) {
    sendError('not_found', `no paired machine ${nodeId}`);
    return null;
  }

  // The pane channel carries the same query the local pane socket does, so the
  // far side applies its own controller rules — one controller per pane holds
  // across the fleet, not merely per machine.
  const params = new URLSearchParams({ mode });
  if (takeover) params.set('takeover', '1');
  const base = federationUrl(row.url).replace(/\/federation$/, '');
  const url = `${base}/federation/pane/${encodeURIComponent(paneId)}?${params}`;

  let peerWs;
  try {
    peerWs = await connectToPeer({
      url,
      peerNodeId: nodeId,
      identity,
      db,
      // Registered before the handshake finishes, so the peer's `replay-start`
      // — which it sends the instant it accepts — cannot arrive with nobody
      // listening. That window is the whole reason this is a callback rather
      // than something attached after the promise resolves.
      onMessage: (raw, isBinary) => {
        if (clientWs.readyState === 1) clientWs.send(raw, { binary: isBinary });
      },
    });
  } catch (e) {
    sendError('not_found', `cannot reach ${row.name || nodeId}: ${e.message}`);
    return null;
  }

  clientWs.on('message', (raw, isBinary) => {
    if (peerWs.readyState === 1) peerWs.send(raw, { binary: isBinary });
  });

  const shutdown = () => {
    try {
      peerWs.close();
    } catch {
      /* already gone */
    }
    try {
      clientWs.close();
    } catch {
      /* already gone */
    }
  };

  // Either end closing takes the pair down. A half-open relay would leave the
  // client watching a pane that stopped existing, with no frame to say so.
  clientWs.on('close', shutdown);
  clientWs.on('error', shutdown);
  peerWs.on('close', shutdown);
  peerWs.on('error', shutdown);

  return peerWs;
}

module.exports = { relayPane };
