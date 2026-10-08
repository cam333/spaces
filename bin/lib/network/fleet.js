/**
 * The fleet as the runtime sees it: which machines exist, and what shape they
 * are reported to a client in.
 *
 * This is the seam between the node store (a SQLite table) and the protocol
 * (`net.peers`, `net.pair`, `net.forget`). It exists so the runtime's dispatch
 * stays a thin translation and so the "no key material reaches the client" rule
 * has exactly one place to be enforced — [`toNetNode`].
 *
 * Opening the database is lazy and failure is not fatal: federation is opt-in,
 * and a machine that has never paired with anything should not be prevented
 * from serving its own panes because `network.db` is missing or unreadable.
 */

const os = require('os');

const nodeStore = require('./nodes');

/** How many characters of a public key are shown for telling keys apart. */
const KEY_HINT_CHARS = 12;

/**
 * The wire shape of one machine. **The only place a database row becomes
 * something a client sees**, and therefore the only place that has to be right
 * about what must not travel: `api_key_encrypted` and the peer's full public
 * key both stop here.
 */
function toNetNode(row, { self = false, status = null } = {}) {
  const paired = Boolean(row.public_key);
  return {
    nodeId: row.id,
    name: row.name || row.id,
    url: row.url || '',
    status: status || (paired ? row.status || 'unknown' : 'unpaired'),
    paired,
    discoveredVia: row.discovered_via || 'manual',
    version: row.version || null,
    lastSeen: row.last_seen || null,
    keyHint: row.public_key ? String(row.public_key).slice(0, KEY_HINT_CHARS) : null,
    self,
  };
}

/**
 * @param {object} opts
 * @param {string} opts.nodeId      this machine's id, so it can mark itself
 * @param {string} [opts.nodeName]  this machine's display name
 * @param {string} [opts.dbPath]    override for tests
 * @param {(event: string, payload: object) => void} [opts.emit]
 * @param {(nodeId: string) => string|null} [opts.statusOf]
 *   Live reachability from the link manager, when there is one. The `status`
 *   column records what was last *persisted*, which is not the same question:
 *   a machine this node is talking to right now still reads `unknown` there,
 *   and a client would render it as forever connecting.
 */
function createFleet({
  nodeId,
  nodeName = os.hostname(),
  dbPath,
  emit = () => {},
  statusOf = () => null,
} = {}) {
  let db = null;
  let unavailable = false;

  function store() {
    if (db || unavailable) return db;
    try {
      db = nodeStore.openNodeDb(dbPath ? { path: dbPath } : {});
    } catch {
      // No peer database is a normal state for a machine that has never
      // federated. Treat it as an empty fleet rather than an error, and stop
      // retrying on every call.
      unavailable = true;
    }
    return db;
  }

  /** This machine, always first, whether or not any peers exist. */
  function selfNode() {
    return toNetNode(
      {
        id: nodeId,
        name: nodeName,
        url: '',
        public_key: null,
        discovered_via: 'self',
        status: 'online',
      },
      { self: true, status: 'online' }
    );
  }

  return {
    /** Every machine, this one first. */
    listNodes() {
      const nodes = [selfNode()];
      const d = store();
      if (!d) return nodes;
      try {
        for (const row of nodeStore.listNodes(d)) {
          if (row.id === nodeId) continue; // never list ourselves twice
          nodes.push(toNetNode(row, { status: statusOf(row.id) }));
        }
      } catch {
        // A malformed peer table must not stop a client seeing its own machine.
      }
      return nodes;
    },

    /**
     * Records a machine as paired.
     *
     * `nodeId` is optional because a machine's id is normally learnt from it
     * during pairing; when the caller already knows it (re-pairing after a key
     * rotation, say) it may be supplied. A pairing with no id and no way to
     * learn one is rejected rather than invented, because the id is the key the
     * trust anchor is stored under.
     */
    pair({ url, publicKey, name, apiKey = '', nodeId: peerNodeId }) {
      const d = store();
      if (!d) throw new Error('peer database is unavailable');
      const id = peerNodeId || deriveIdFromKey(publicKey);
      const row = nodeStore.pairNode(d, {
        id,
        name: name || id,
        url,
        publicKey,
        apiKey,
        discoveredVia: 'manual',
      });
      const node = toNetNode(row);
      emit('net.node.updated', { node });
      return node;
    },

    forget(peerNodeId) {
      const d = store();
      if (!d) return false;
      const removed = nodeStore.forgetNode(d, peerNodeId);
      if (removed) emit('net.node.updated', { nodeId: peerNodeId, removed: true });
      return removed;
    },

    /** The stored trust anchor for a peer, for the connecting side. */
    publicKeyOf(peerNodeId) {
      const d = store();
      if (!d) return null;
      const row = nodeStore.getNode(d, peerNodeId);
      return row ? row.public_key : null;
    },

    get db() {
      return store();
    },
  };
}

/**
 * A stable id derived from a public key, for pairing where the machine's own id
 * is not yet known.
 *
 * Deterministic, so pairing the same key twice updates one row rather than
 * accumulating duplicates. Not a substitute for the machine's real id — it is
 * only used when none was supplied.
 */
function deriveIdFromKey(publicKey) {
  const crypto = require('crypto');
  return 'key-' + crypto.createHash('sha256').update(String(publicKey)).digest('hex').slice(0, 16);
}

module.exports = { createFleet, toNetNode, deriveIdFromKey, KEY_HINT_CHARS };
