'use strict';

// Which port this account's Spaces server uses.
//
// Every account on a machine runs its own server under its own secret, so
// two people using `spaces --remote` against the same host need a port each.
// The default only goes to the first account to take it; any other account
// claims the next free port (with the internal port below it also free) and
// saves it, so it stays the same on every later start and connect.

const DEFAULT_PORT = 3457;
/** The internal Next.js port sits this far below the main one. */
const INTERNAL_PORT_OFFSET = 57;
/** How far above the default to look for a free pair. */
const SEARCH_SPAN = 200;

/**
 * @param {object} opts
 * @param {number|undefined} opts.chosen - a port from --port, SPACES_PORT or server.json
 * @param {(port: number) => boolean} opts.isMine - false when another account listens there
 * @param {(port: number) => boolean} opts.isListening - anything listening there at all
 * @param {(port: number) => void} opts.save - remember a claimed port
 * @returns {{ port: number, claimed: boolean }}
 */
function resolveAccountPort({ chosen, isMine, isListening, save, defaultPort = DEFAULT_PORT }) {
  if (chosen) return { port: chosen, claimed: false };
  if (isMine(defaultPort)) return { port: defaultPort, claimed: false };
  for (let port = defaultPort + 1; port < defaultPort + SEARCH_SPAN; port++) {
    if (isListening(port) || isListening(port - INTERNAL_PORT_OFFSET)) continue;
    try { save(port); } catch { /* unsaved, it is still this run's port */ }
    return { port, claimed: true };
  }
  // Nothing free: the default, which then fails at the ownership check
  // rather than quietly connecting somewhere else.
  return { port: defaultPort, claimed: false };
}

module.exports = { resolveAccountPort, DEFAULT_PORT, INTERNAL_PORT_OFFSET };
