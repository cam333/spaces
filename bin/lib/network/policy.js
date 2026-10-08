/**
 * What a federated peer is allowed to ask for.
 *
 * A federated session is deliberately weaker than a local one. The point is
 * that a credential stolen from a peer is worth less than one stolen from the
 * machine itself — an attacker who gets one should find a confined tool rather
 * than a shell.
 *
 * Two rules do most of the work:
 *
 *   - **Allow-list, never deny-list.** A method added to the runtime later is
 *     unavailable over federation until somebody decides it is safe. The
 *     alternative silently exposes every new capability the day it lands.
 *   - **Fail closed.** No configured dev directories means nothing may be
 *     started, not everything.
 */

const path = require('path');

/**
 * The methods a peer may call, and whether each changes anything.
 *
 * `kind` is what the read-only peer setting keys off, so a method with no
 * classification would slip past it — the shape of that mistake is why a test
 * asserts every entry has one.
 */
const FEDERATED_METHODS = {
  'session.snapshot': { kind: 'read' },
  'node.info': { kind: 'read' },
  'node.methods': { kind: 'read' },
  'events.subscribe': { kind: 'read' },
  'pane.create': { kind: 'write' },
  'pane.close': { kind: 'write' },
  'pane.restart': { kind: 'write' },
  'pane.update': { kind: 'write' },
  'pane.input': { kind: 'write' },
  'pane.resize': { kind: 'write' },
  'workspace.create': { kind: 'write' },
  'workspace.update': { kind: 'write' },
  'workspace.remove': { kind: 'write' },
};

/** Params that must never be honoured from a peer, whatever the method. */
const FORBIDDEN_PARAMS = ['customCommand'];

function refuse(reason) {
  return { allowed: false, reason };
}

/**
 * `true` if `candidate` is `root` or lives beneath it.
 *
 * Resolves both sides first, so `C:\projects\..\Windows` is compared as
 * `C:\Windows`. The trailing separator matters: without it `C:\projects-evil`
 * passes a prefix test against `C:\projects` while being nowhere near it.
 */
function isInside(root, candidate) {
  const resolvedRoot = path.resolve(root);
  const resolved = path.resolve(candidate);
  if (resolved === resolvedRoot) return true;
  const withSep = resolvedRoot.endsWith(path.sep) ? resolvedRoot : resolvedRoot + path.sep;
  // Case-insensitive on Windows, where C:\Projects and C:\projects are one
  // directory and a case-sensitive compare would refuse a legitimate path.
  if (process.platform === 'win32') {
    return resolved.toLowerCase().startsWith(withSep.toLowerCase());
  }
  return resolved.startsWith(withSep);
}

/**
 * Decides whether a peer's request may proceed.
 *
 * Returns `{ allowed, reason }` rather than throwing: the reason is written to
 * the audit log and returned to the peer, and an exception would make both of
 * those the caller's problem.
 */
function checkFederatedRequest({ method, params = {}, devDirs = [], readOnly = false }) {
  const spec = FEDERATED_METHODS[method];
  if (!spec) {
    return refuse(`method "${method}" is not available over federation`);
  }

  if (readOnly && spec.kind === 'write') {
    return refuse(`this peer is paired read-only, and "${method}" changes state`);
  }

  for (const forbidden of FORBIDDEN_PARAMS) {
    if (params[forbidden] !== undefined && params[forbidden] !== null) {
      return refuse(
        `"${forbidden}" is never accepted from a peer — it names an arbitrary program to run`
      );
    }
  }

  // Anything that starts a process is confined to the directories the user
  // nominated as development roots.
  if (typeof params.cwd === 'string' && params.cwd.length > 0) {
    if (devDirs.length === 0) {
      return refuse(
        'no development directories are configured, so no directory is permitted for a federated pane'
      );
    }
    if (!devDirs.some((root) => isInside(root, params.cwd))) {
      return refuse(
        `"${params.cwd}" is outside every configured development directory`
      );
    }
  }

  return { allowed: true };
}

module.exports = { checkFederatedRequest, FEDERATED_METHODS, FORBIDDEN_PARAMS, isInside };
