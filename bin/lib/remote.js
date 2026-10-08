'use strict';

/**
 * `spaces --remote <destination>`: the terminal client against a Spaces
 * server on another machine, carried over SSH.
 *
 * The remote server keeps listening on its own loopback only. `ssh` runs
 * `spaces start` there, then forwards a local port to the remote server's,
 * and the client connects to the local end exactly as it would to a local
 * server. Authentication is whatever SSH already does for this destination
 * — keys, agent, ~/.ssh/config aliases, jump hosts — so this adds no
 * listener, no token and no credential of its own: anyone who can use it
 * could already open a shell there.
 */

/** What `spaces start` prints so the caller can find the server's port. */
const PORT_LINE = /^SPACES_PORT=(\d+)\s*$/m;

/**
 * What `spaces start --print-token` prints: a signed session token for the
 * remote server, never its secret, so the secret stays on that machine.
 */
const TOKEN_LINE = /^SPACES_TOKEN=([A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)\s*$/m;

/**
 * Throws unless `dest` is something `ssh` will read as a destination and
 * nothing else: `[user@]host`, a `~/.ssh/config` alias, or an
 * `ssh://[user@]host[:port]` URI.
 *
 * A leading `-` is the dangerous case: `ssh` would parse it as an option,
 * and `-oProxyCommand=...` runs an arbitrary local command. The caller also
 * puts `--` before the destination, so this is the first of two guards.
 */
function validateDestination(dest) {
  if (typeof dest !== 'string' || !dest) throw new Error('--remote needs a destination, e.g. you@host');
  if (dest.startsWith('-')) throw new Error(`not a destination: ${dest}`);
  const plain = /^(?:[A-Za-z0-9._-]+@)?[A-Za-z0-9._-]+$/;
  const uri = /^ssh:\/\/(?:[A-Za-z0-9._-]+@)?(?:[A-Za-z0-9._-]+|\[[0-9A-Fa-f:.]+\])(?::\d{1,5})?$/;
  if (!plain.test(dest) && !uri.test(dest)) throw new Error(`not a destination: ${dest}`);
  return dest;
}

/**
 * `ssh` arguments that run `spaces start` on the remote. The remote command
 * is a constant: nothing the user typed reaches the remote shell.
 *
 * A non-interactive SSH session often lacks the PATH an interactive one has,
 * so `loginShell` retries through the user's login shell. nvm needs more
 * than that: its installer adds itself to ~/.bashrc *after* the stock
 * "not interactive, stop here" guard, so neither kind of non-interactive
 * shell ever loads it. The retry sources nvm itself when it is there.
 */
const LOGIN_RETRY =
  `"\${SHELL:-sh}" -lc '[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1; spaces start --print-token'`;

function sshStartArgs(dest, { loginShell = false } = {}) {
  const command = loginShell ? LOGIN_RETRY : 'spaces start --print-token';
  return ['-o', 'ConnectTimeout=15', '--', dest, command];
}

/** `ssh` arguments for the tunnel: local port → the remote server's loopback. */
function sshTunnelArgs(dest, localPort, remotePort) {
  return [
    '-N',
    '-o', 'ExitOnForwardFailure=yes',
    '-o', 'ServerAliveInterval=30',
    '-o', 'ConnectTimeout=15',
    '-L', `127.0.0.1:${localPort}:127.0.0.1:${remotePort}`,
    '--', dest,
  ];
}

/** The port `spaces start` reported, or null. */
function parseStartOutput(stdout) {
  const m = PORT_LINE.exec(String(stdout));
  if (!m) return null;
  const port = Number(m[1]);
  return port >= 1 && port <= 65535 ? port : null;
}

/** The session token `spaces start --print-token` reported, or null. */
function parseStartToken(stdout) {
  const m = TOKEN_LINE.exec(String(stdout));
  return m ? m[1] : null;
}

/**
 * Why `ssh <dest> spaces start` failed, from its exit status and stderr:
 * `'ssh'` (could not connect or authenticate — ssh exits 255),
 * `'missing'` (the remote shell could not find `spaces`), or `'failed'`.
 */
function classifyStartFailure(status, stderr) {
  if (status === 255) return 'ssh';
  if (status === 127 || /spaces: (command )?not found|not recognized as an internal or external command/i.test(String(stderr))) {
    return 'missing';
  }
  return 'failed';
}

/** The message for a remote without Spaces on it. */
function installHint(dest) {
  return [
    `  Spaces is not installed on ${dest}, or is not on the PATH a`,
    '  non-interactive SSH session gets. To install it there:',
    '',
    `    ssh ${dest} 'npm install -g @jlongo78/agent-spaces'`,
    '',
    '  It needs Node.js 20.9 or newer on that machine.',
  ].join('\n');
}

/**
 * Splits `--remote <dest>` out of argv. A trailing `--remote` with no
 * value is left in `rest` so it is reported, not silently dropped.
 */
function splitRemote(argv) {
  const rest = [];
  let remote = null;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--remote' && i + 1 < argv.length) {
      remote = argv[++i];
      continue;
    }
    rest.push(argv[i]);
  }
  return { remote, rest };
}

module.exports = {
  validateDestination,
  sshStartArgs,
  sshTunnelArgs,
  parseStartOutput,
  parseStartToken,
  classifyStartFailure,
  installHint,
  splitRemote,
};
