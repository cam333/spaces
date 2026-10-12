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
 * PATH an interactive session would have — and ~/.local/bin first on it,
 * where `--remote` installs Spaces when npm's own global folder belongs to
 * root. `command` is spliced inside single quotes, so it must never contain
 * one; every caller passes a constant or something built only from
 * validated parts.
 */
function inLoginShell(command) {
  if (command.includes("'")) throw new Error('a remote command cannot contain a single quote');
  return `"\${SHELL:-sh}" -lc '[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1; PATH="$HOME/.local/bin:$PATH"; ${command}'`;
}

function sshStartArgs(dest, { loginShell = false, login = false } = {}) {
  const start = `spaces start --print-token${login ? ' --print-login' : ''}`;
  return ['-o', 'ConnectTimeout=15', '--', dest, loginShell ? inLoginShell(start) : start];
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

function sshProbeArgs(dest, { batch = false } = {}) {
  return [...(batch ? ['-o', 'BatchMode=yes'] : []), '-o', 'ConnectTimeout=15', '--', dest, PROBE];
}

// ─── Signing in with a key ──────────────────────────────────

/** The private keys plain `ssh` tries by default, in the order it tries them. */
const DEFAULT_KEYS = ['id_ed25519', 'id_ecdsa', 'id_rsa'];

/**
 * Why a no-prompt (`BatchMode`) ssh failed: `'password'` (the server wants a
 * password, or another key), `'hostkey'` (ssh would have asked about an
 * unknown host key), or `'other'`.
 */
function classifySshFailure(stderr) {
  const text = String(stderr);
  if (/Host key verification failed|REMOTE HOST IDENTIFICATION HAS CHANGED/i.test(text)) return 'hostkey';
  if (/Permission denied/i.test(text)) return 'password';
  return 'other';
}

/**
 * One public key line, as `ssh-keygen` writes it, or null. It travels to the
 * remote on stdin rather than in the command, but is still checked: it is
 * about to be trusted for every future sign-in there.
 */
function publicKeyLine(text) {
  const line = String(text).split(/\r?\n/)[0].trim();
  return /^(ssh-ed25519|ecdsa-sha2-nistp(256|384|521)|ssh-rsa|sk-ssh-ed25519@openssh\.com) [A-Za-z0-9+/]+=*( [^\r\n]*)?$/.test(line)
    ? line
    : null;
}

/**
 * Adds the public key on stdin to ~/.ssh/authorized_keys on the remote,
 * once, creating the file with the permissions sshd insists on. `sh -c`
 * so it runs the same whatever the remote user's own shell is.
 */
const AUTHORIZE_KEY = `sh -c 'umask 077; mkdir -p "$HOME/.ssh" && k=$(cat) && touch "$HOME/.ssh/authorized_keys" && { grep -qxF "$k" "$HOME/.ssh/authorized_keys" || printf "%s\\n" "$k" >> "$HOME/.ssh/authorized_keys"; }'`;

function sshAuthorizeKeyArgs(dest) {
  return ['-o', 'ConnectTimeout=15', '--', dest, AUTHORIZE_KEY];
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
 * Installs `spec` on the remote and starts it, in one SSH session — each
 * session can mean another password prompt. Into npm's global folder when
 * this account can write there (nvm's always can), else into ~/.local,
 * which needs no root. For an update, `spaces restart` first puts the new
 * version in charge: the freshly installed one, which knows to go through
 * the service when there is one. Ends with what `spaces start
 * --print-token` prints, so the caller need not ask again.
 */
function sshInstallArgs(dest, spec, { restart = false, login = false } = {}) {
  const command = [
    `if [ -w "$(npm prefix -g)/lib" ]; then npm install -g ${spec};`,
    `else npm install -g --prefix "$HOME/.local" ${spec}; fi`,
    restart ? '&& spaces restart' : '',
    `&& spaces start --print-token${login ? ' --print-login' : ''}`,
  ].filter(Boolean).join(' ');
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

/**
 * `ssh` arguments for the tunnel: local port → the remote server's loopback.
 * `-v` so its log says when the forward is up (see tunnelReady).
 */
function sshTunnelArgs(dest, localPort, remotePort) {
  return [
    '-v',
    '-N',
    '-o', 'ExitOnForwardFailure=yes',
    '-o', 'ServerAliveInterval=30',
    '-o', 'ConnectTimeout=15',
    '-L', `127.0.0.1:${localPort}:127.0.0.1:${remotePort}`,
    '--', dest,
  ];
}

/**
 * Whether the tunnel's `ssh -v` log says the forward from `localPort` is
 * listening. Waiting on the log rather than probing the port: a probe is a
 * child process, and on Windows each one that exits resets the console,
 * which garbles a password being typed into ssh's prompt meanwhile.
 */
function tunnelReady(log, localPort) {
  const port = String(localPort).replace(/\D/g, '');
  return new RegExp(`Local forwarding listening on 127\\.0\\.0\\.1 port ${port}\\b|Local connections to 127\\.0\\.0\\.1:${port} forwarded`).test(String(log));
}

/** Whether the tunnel's log shows ssh got past authentication. */
function tunnelAuthenticated(log) {
  return /^Authenticated to /m.test(String(log));
}

/** An `ssh -v` log without its debug chatter, for showing a failure. */
function withoutDebug(log) {
  return String(log).split(/\r?\n/).filter((l) => !/^(debug\d:|OpenSSH_)/.test(l)).join('\n');
}

/** The port `spaces start` reported, or null. */
function parseStartOutput(stdout) {
  const m = PORT_LINE.exec(String(stdout));
  if (!m) return null;
  const port = Number(m[1]);
  return port >= 1 && port <= 65535 ? port : null;
}

/** The browser sign-in code `spaces start --print-login` reported, or null. */
function parseStartLogin(stdout) {
  const m = /^SPACES_LOGIN=([A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)\s*$/m.exec(String(stdout));
  return m ? m[1] : null;
}

/** The path the remote serves the web app under, or '' (also when malformed). */
function parseStartBasePath(stdout) {
  const m = /^SPACES_BASE_PATH=(\/[A-Za-z0-9._~\/-]*?)\/?\s*$/m.exec(String(stdout));
  return m ? m[1] : '';
}

/**
 * The local port `spaces open --remote` tunnels from, the same each time for
 * the same destination, so the address stays put in the browser's history.
 * Within 41000–41999; another is used when this one is taken.
 */
function preferredLocalPort(dest) {
  let h = 0;
  for (const c of String(dest)) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return 41000 + (h % 1000);
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
  parseStartLogin,
  parseStartBasePath,
  preferredLocalPort,
  classifyStartFailure,
  installHint,
  splitRemote,
  PACKAGE,
  DEFAULT_KEYS,
  classifySshFailure,
  publicKeyLine,
  sshAuthorizeKeyArgs,
  inLoginShell,
  tunnelReady,
  tunnelAuthenticated,
  withoutDebug,
  sshProbeArgs,
  parseProbe,
  nodeIsSupported,
  planRemote,
  installSpec,
  sshInstallArgs,
  classifyInstallFailure,
  installFailureHint,
};
