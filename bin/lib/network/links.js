/**
 * Outbound links to paired machines.
 *
 * The local node connects to peers; the client never does. That is a security
 * decision before it is an architectural one — the private key and the pinned
 * records live here, so the client that talks to the world holds nothing worth
 * stealing.
 *
 * Each link is a `/proto` control channel riding the authenticated socket
 * `connectToPeer` returns, so a peer is driven with the exact protocol a local
 * client uses. There is no second dialect to keep in step.
 *
 * Reconnection is deliberately lazy rather than eager: a link is dialled when
 * something asks for that machine and re-dialled on the next ask after it
 * drops. A background reconnect loop against an offline machine buys nothing —
 * nobody is looking at it — and costs a timer per peer forever.
 */

const { connectToPeer } = require('./client');
const { listNodes } = require('./nodes');

/** How long a peer has to answer one request. */
const REQUEST_TIMEOUT_MS = 10_000;

/** Wait after a failure before a peer is worth dialling again. */
const RETRY_COOLDOWN_MS = 5_000;

/**
 * How often an open link is probed.
 *
 * Symmetrical with the listener's sweep, and needed for the same reason: a
 * federated link is idle for long stretches by design, so silence proves
 * nothing and only a probe distinguishes a quiet peer from a vanished one.
 * Without it a half-open link is discovered only when something finally sends
 * a request and waits out the full timeout.
 */
const HEARTBEAT_MS = 30_000;

function createPeerLinks({ identity, db, now = Date.now, onEvent = () => {}, onStatus = () => {} }) {
  /** nodeId -> { ws, pending: Map, nextId, helloDone } */
  const links = new Map();
  /** nodeId -> timestamp before which we will not retry */
  const cooldowns = new Map();
  /** nodeId -> last reported reachability, for anyone rendering the fleet */
  const statuses = new Map();

  /** Records reachability and tells whoever is listening. */
  const report = (nodeId, status) => {
    statuses.set(nodeId, status);
    onStatus(nodeId, status);
  };

  /**
   * Tears a link down and says what that means.
   *
   * `status` distinguishes the two cases a single "offline" used to blur: a
   * link that ended cleanly leaves the machine **idle** — links are lazy, so
   * having none is the normal resting state for a machine nobody is looking
   * at — whereas one that failed leaves it **offline**. Reporting a healthy
   * idle machine as offline is what made a peer look down when it was merely
   * quiet, and no amount of refreshing could correct it because nothing had
   * gone wrong to correct.
   */
  function drop(nodeId, reason, status = 'offline') {
    const link = links.get(nodeId);
    if (!link) return;
    links.delete(nodeId);
    for (const { reject } of link.pending.values()) {
      reject(new Error(`link to ${nodeId} closed: ${reason}`));
    }
    link.pending.clear();
    try {
      link.ws.close();
    } catch {
      /* already gone */
    }
    report(nodeId, status);
  }

  function attach(nodeId, ws) {
    const link = { ws, pending: new Map(), nextId: 0 };
    links.set(nodeId, link);

    ws.on('message', (raw) => {
      let msg;
      try {
        msg = JSON.parse(raw.toString());
      } catch {
        return;
      }
      // An event frame: forward it, tagged with the machine it came from, so a
      // client can fold it into that machine's slot.
      if (msg.event) {
        onEvent(nodeId, msg.event);
        return;
      }
      if (msg.type === 'hello-ok') {
        link.helloDone = true;
        return;
      }
      const waiter = link.pending.get(String(msg.id));
      if (!waiter) return;
      link.pending.delete(String(msg.id));
      clearTimeout(waiter.timer);
      msg.error ? waiter.reject(new Error(`${msg.error.code}: ${msg.error.message}`)) : waiter.resolve(msg.result);
    });

    // A clean close is not a failure: the machine is simply not linked now.
    ws.on('close', () => drop(nodeId, 'closed', 'idle'));
    ws.on('error', () => drop(nodeId, 'error', 'offline'));
    ws.isAlive = true;
    ws.on('pong', () => {
      ws.isAlive = true;
    });

    ws.send(JSON.stringify({ type: 'hello', protocolVersion: 1, client: 'spaces-federation' }));
    return link;
  }

  async function linkTo(nodeId) {
    const existing = links.get(nodeId);
    if (existing && existing.ws.readyState === 1) return existing;

    const until = cooldowns.get(nodeId) || 0;
    if (now() < until) throw new Error(`${nodeId} is in a retry cooldown`);

    const row = listNodes(db).find((n) => n.id === nodeId);
    if (!row) throw new Error(`no record of ${nodeId}`);
    if (!row.public_key) throw new Error(`${nodeId} has never been paired`);

    report(nodeId, 'connecting');
    try {
      const ws = await connectToPeer({
        url: federationUrl(row.url),
        peerNodeId: nodeId,
        identity,
        db,
      });
      const link = attach(nodeId, ws);
      report(nodeId, 'online');
      return link;
    } catch (e) {
      cooldowns.set(nodeId, now() + RETRY_COOLDOWN_MS);
      report(nodeId, 'offline');
      throw e;
    }
  }

  // Probes every open link, dropping any that missed the last round trip.
  const heartbeat = setInterval(() => {
    for (const [nodeId, link] of links) {
      if (link.ws.isAlive === false) {
        drop(nodeId, 'no response to ping', 'offline');
        continue;
      }
      link.ws.isAlive = false;
      try {
        link.ws.ping();
      } catch {
        drop(nodeId, 'ping failed', 'offline');
      }
    }
  }, HEARTBEAT_MS);
  if (heartbeat.unref) heartbeat.unref();

  return {
    /** Sends one request to a peer, dialling it first if needed. */
    async request(nodeId, method, params = {}) {
      const link = await linkTo(nodeId);
      const id = String(++link.nextId);
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          link.pending.delete(id);
          reject(new Error(`${nodeId} did not answer ${method} in time`));
        }, REQUEST_TIMEOUT_MS);
        link.pending.set(id, { resolve, reject, timer });
        try {
          link.ws.send(JSON.stringify({ id, method, params }));
        } catch (e) {
          clearTimeout(timer);
          link.pending.delete(id);
          reject(e);
        }
      });
    },

    /** Subscribes to a peer's event stream so its changes reach this node. */
    async follow(nodeId) {
      return this.request(nodeId, 'events.subscribe', { afterSeq: 0 });
    },

    /** Last known reachability, or `null` if never attempted. */
    statusOf(nodeId) {
      return statuses.get(nodeId) || null;
    },

    isLinked(nodeId) {
      const link = links.get(nodeId);
      return Boolean(link && link.ws.readyState === 1);
    },

    close() {
      clearInterval(heartbeat);
      for (const nodeId of [...links.keys()]) drop(nodeId, 'shutting down', 'idle');
    },
  };
}

/**
 * Turns a peer's stored URL into its federation WebSocket URL.
 *
 * The stored URL is the machine's web address — how it was reached before
 * federation had a listener of its own. The federation port is separate by
 * design, so this rewrites scheme and path and takes the port from the URL
 * when one is given.
 */
function federationUrl(url, defaultPort = 3479) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return url;
  }
  // `new URL` is happy to parse "gpu-box:3479" as protocol `gpu-box:` with no
  // hostname at all, which would rewrite to `ws://:3479/federation` — a URL
  // that points nowhere and fails with a confusing error far from here. A
  // parse with no host is not a URL for this purpose; hand it back untouched
  // so it fails at the socket saying what the user actually typed.
  if (!parsed.hostname) return url;
  const scheme = parsed.protocol === 'https:' ? 'wss:' : 'ws:';
  const port = parsed.port && parsed.port !== '' ? parsed.port : String(defaultPort);
  return `${scheme}//${parsed.hostname}:${port}/federation`;
}

module.exports = { createPeerLinks, federationUrl, REQUEST_TIMEOUT_MS, RETRY_COOLDOWN_MS };
