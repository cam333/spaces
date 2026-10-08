'use strict';

/**
 * The experimental gate for multiuser mode and federation.
 *
 * A security review found both unsafe for general use: in multiuser mode any
 * signed-in user can currently gain control of the account the server runs
 * as, and a federated (paired) machine can act as you over a channel that is
 * not encrypted. Both therefore stay off unless the person starting the
 * server opts in for that run, with `--experimental` or
 * `SPACES_EXPERIMENTAL=1`.
 *
 * There is deliberately no saved (server.json) form of the opt-in: a setting
 * written once and forgotten would turn the risk back on for every later
 * start, which is exactly how a saved `multiuser: true` used to work.
 *
 * Everything here is pure, so the launcher's decisions can be tested without
 * starting a server.
 */

/** Whether the risky features may run: the CLI flag, or SPACES_EXPERIMENTAL. */
function experimentalEnabled({ flag, env = {} } = {}) {
  if (flag === true) return true;
  const v = String(env.SPACES_EXPERIMENTAL || '').trim().toLowerCase();
  return v === '1' || v === 'true';
}

/**
 * Whether the server would run in multiuser mode, and why. Same precedence
 * the launcher has always used: an explicit flag (either direction) wins,
 * then SPACES_MULTIUSER=1, then the saved setting. `source` names where the
 * request came from, so a refusal can say what to change.
 */
function multiuserRequest({ flag, env = {}, saved = {} } = {}) {
  if (flag === true) return { multiuser: true, source: 'the --multiuser flag' };
  if (flag === false) return { multiuser: false, source: null };
  if (env.SPACES_MULTIUSER === '1') return { multiuser: true, source: 'SPACES_MULTIUSER=1' };
  if (saved && saved.multiuser === true) {
    return { multiuser: true, source: '"multiuser": true in ~/.spaces/server.json' };
  }
  return { multiuser: false, source: null };
}

/**
 * `{ allowed: true }`, or `{ allowed: false, lines }` with the message to
 * print before refusing to start.
 */
function gateMultiuser({ multiuser, source, experimental }) {
  if (!multiuser || experimental) return { allowed: true };
  return {
    allowed: false,
    lines: [
      `  Refusing to start in multiuser mode (requested by ${source || 'your configuration'}).`,
      '',
      '  Multiuser mode is experimental and not safe to use yet: any signed-in',
      '  user can currently gain control of the account the server runs as.',
      '',
      '  Use desktop mode instead (the default, localhost only): run "spaces",',
      '  or "spaces --desktop" to override a saved multiuser setting.',
      '  To accept the risk anyway, pass --experimental (or set SPACES_EXPERIMENTAL=1).',
    ],
  };
}

/**
 * Whether to start the federation listener and peer links for `iface`.
 * `{ start: false }` when federation was not asked for; `{ start: false,
 * lines }` when it was but the experimental opt-in is missing.
 */
function gateFederation({ iface, experimental }) {
  if (!iface) return { start: false };
  if (experimental) return { start: true };
  return {
    start: false,
    lines: [
      `  Federation is configured (${iface}) but NOT started: it is experimental.`,
      '  A paired machine can act as you on this one, and federation traffic is',
      '  not encrypted. Start with --experimental (or SPACES_EXPERIMENTAL=1) to',
      '  enable it anyway.',
    ],
  };
}

/** The note `spaces whoami` prints alongside pairing details. */
const FEDERATION_WHOAMI_NOTE = [
  '  Note: federation is experimental. A paired machine can act as you, and',
  '  the traffic is not encrypted. The listener only starts when the server',
  '  is started with --experimental (or SPACES_EXPERIMENTAL=1).',
];

module.exports = {
  experimentalEnabled,
  multiuserRequest,
  gateMultiuser,
  gateFederation,
  FEDERATION_WHOAMI_NOTE,
};
