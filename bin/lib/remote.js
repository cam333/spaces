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
/**
 * `command` run through the remote user's login shell with nvm loaded — the
 * PATH an interactive session would have. `command` is spliced inside single
 * quotes, so it must never contain one; every caller passes a constant or
 * something built only from validated parts.
 */
function inLoginShell(command) {
  if (command.includes("'")) throw new Error('a remote command cannot contain a single quote');
  return `"\${SHELL:-sh}" -lc '[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1; ${command}'`;
}

function sshStartArgs(dest, { loginShell = false } = {}) {
  const command = loginShell ? inLoginShell('spaces start --print-token') : 'spaces start --print-token';
  return ['-o', 'ConnectTimeout=15', '--', dest, command];
}

// ─── Installing and updating the remote ────────────────────

/** The package `--remote` installs on the other machine. */
const PACKAGE = '@jlongo78/agent-spaces';

/** The oldest Node.js the package runs on. */
const MIN_NODE = [20, 9];

/**
 * What the remote has: its Node.js, its Spaces version, and — from Spaces
 * versions that can say — whether the server runs and how many agents are
 * live in it. Asking `spaces status` only of versions that have it: an old
 * enough `spaces` treated an unknown command as "start the server".
 */
const PROBE = inLoginShell([
  'command -v node >/dev/null 2>&1 && echo "SPACES_NODE=$(node -v)";',
  'if command -v spaces >/dev/null 2>&1; then',
  '  v=$(spaces --version 2>/dev/null); echo "SPACES_VERSION=$v";',
  '  case "$v" in 0.*|"") ;; *) spaces status --porcelain 2>/dev/null ;; esac;',
  'else echo SPACES_MISSING=1; fi',
].join(' '));

function sshProbeArgs(dest) {
  return ['-o', 'ConnectTimeout=15', '--', dest, PROBE];
}

const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

/**
 * What the probe found, or null when it found nothing it recognises — a
 * remote whose shell is not sh-like (Windows, fish) answers that way, and
 * `--remote` then connects exactly as it did before installs existed.
 */
function parseProbe(stdout) {
  const text = String(stdout);
  const line = (key) => {
    const m = new RegExp(`^${key}=(.*?)\\s*$`, 'm').exec(text);
    return m ? m[1] : null;
  };
  const missing = line('SPACES_MISSING') === '1';
  const versionLine = line('SPACES_VERSION');
  if (!missing && versionLine === null) return null;
  const node = (line('SPACES_NODE') || '').replace(/^v/, '');
  const version = /\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?/.exec(versionLine || '');
  const running = line('SPACES_RUNNING');
  const live = line('SPACES_LIVE_AGENTS');
  return {
    node: /^\d+\.\d+\.\d+/.test(node) ? node : null,
    missing,
    version: version ? version[0] : null,
    running: running === null ? null : running === '1',
    liveAgents: live !== null && /^\d+$/.test(live) ? Number(live) : null,
  };
}

/** True when `node` (e.g. "20.19.6") is new enough for the package. */
function nodeIsSupported(node) {
  if (!node) return false;
  const [major, minor] = node.split('.').map((n) => parseInt(n, 10));
  return major > MIN_NODE[0] || (major === MIN_NODE[0] && minor >= MIN_NODE[1]);
}

/**
 * What to do about the remote's install, given this machine's version:
 * `connect` (same version, or no probe), `install`, `update` (older there),
 * `newer` (newer there: connect, and say so), `no-node` or `old-node`.
 */
function planRemote(probe, localVersion) {
  const { isNewer } = require('./version');
  if (!probe) return { action: 'connect' };
  if (probe.missing || !probe.version) {
    if (!probe.node) return { action: 'no-node' };
    if (!nodeIsSupported(probe.node)) return { action: 'old-node', node: probe.node };
    return { action: 'install' };
  }
  if (isNewer(localVersion, probe.version)) {
    if (!nodeIsSupported(probe.node)) return { action: 'old-node', node: probe.node };
    return { action: 'update', from: probe.version, running: probe.running, liveAgents: probe.liveAgents };
  }
  if (isNewer(probe.version, localVersion)) return { action: 'newer', remote: probe.version };
  return { action: 'connect' };
}

/**
 * What to install there: this machine's version, by default. A test can
 * name a tarball already on the remote instead, through
 * SPACES_REMOTE_PACKAGE; either way it is spliced into a remote command, so
 * it must be a plain version or path with nothing a shell would act on.
 */
function installSpec(localVersion, override) {
  if (override) {
    if (!/^[A-Za-z0-9@/._~:-]+$/.test(override)) throw new Error(`not a package to install: ${override}`);
    return override;
  }
  if (!SEMVER.test(String(localVersion))) throw new Error(`not a version to install: ${localVersion}`);
  return `${PACKAGE}@${localVersion}`;
}

/**
 * Installs `spec` on the remote; for an update, `spaces restart` then puts
 * the new version in charge — the freshly installed one, which knows to go
 * through the service when there is one.
 */
function sshInstallArgs(dest, spec, { restart = false } = {}) {
  const command = `npm install -g ${spec}${restart ? ' && spaces restart' : ''}`;
  return ['-o', 'ConnectTimeout=15', '--', dest, inLoginShell(command)];
}

/** Why the remote's `npm install` failed, from its output. */
function classifyInstallFailure(output) {
  const text = String(output);
  if (/EACCES|permission denied/i.test(text)) return 'permission';
  if (/ETARGET|E404|No matching version|is not in this registry/i.test(text)) return 'unpublished';
  if (/gyp ERR|node-gyp|make: not found|not found: make|python3? .*not found|g\+\+: not found/i.test(text)) return 'build';
  if (/EEXIST/.test(text)) return 'conflict';
  return 'failed';
}

/** What to tell the user about a failed remote install. */
function installFailureHint(kind, dest, spec) {
  switch (kind) {
    case 'permission':
      return [
        `  npm on ${dest} cannot write to its global folder. Either install Node.js`,
        '  with nvm there (it keeps global packages in your home), or point npm at',
        `  your home:  ssh ${dest} 'npm config set prefix ~/.npm-global'`,
        '  and add ~/.npm-global/bin to the PATH.',
      ].join('\n');
    case 'unpublished':
      return `  ${spec} is not on the npm registry. Install a published version on ${dest} yourself.`;
    case 'build':
      return [
        `  The terminal library could not be compiled on ${dest}. It needs python3,`,
        '  make and a C++ compiler; on Debian or Ubuntu:',
        `    ssh ${dest} 'sudo apt install build-essential python3'`,
      ].join('\n');
    case 'conflict':
      return `  Another package already owns the "spaces" command on ${dest}. Remove it there (npm ls -g) and try again.`;
    default:
      return `  Installing ${spec} on ${dest} failed.`;
  }
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
  PACKAGE,
  inLoginShell,
  sshProbeArgs,
  parseProbe,
  nodeIsSupported,
  planRemote,
  installSpec,
  sshInstallArgs,
  classifyInstallFailure,
  installFailureHint,
};
