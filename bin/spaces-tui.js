#!/usr/bin/env node
'use strict';

const path = require('path');
const fs = require('fs');

const TARGETS = {
  'linux-x64': '@jlongo78/agent-spaces-tui-linux-x64',
  'linux-arm64': '@jlongo78/agent-spaces-tui-linux-arm64',
  'darwin-x64': '@jlongo78/agent-spaces-tui-darwin-x64',
  'darwin-arm64': '@jlongo78/agent-spaces-tui-darwin-arm64',
  'win32-x64': '@jlongo78/agent-spaces-tui-win32-x64',
};

/** The npm package holding this platform's binary, or null if unsupported. */
function platformPackageName(platform = process.platform, arch = process.arch) {
  return TARGETS[`${platform}-${arch}`] || null;
}

function binaryFileName(platform = process.platform) {
  return platform === 'win32' ? 'spaces-tui.exe' : 'spaces-tui';
}

/**
 * The one wording for "this platform has no terminal client". Both entry
 * points (this file's CLI mode and `spaces tui`) print it; it used to be
 * duplicated between them, differing only by indentation.
 */
function noBinaryMessage(platform = process.platform, arch = process.arch) {
  const pkg = platformPackageName(platform, arch);
  if (pkg) {
    // A supported platform with no binary means npm skipped the optional
    // dependency (--omit=optional, --no-optional, or a failed fetch), not
    // that the platform is unsupported.
    return (
      `The terminal client for ${platform}-${arch} (${pkg}) was not installed. ` +
      'npm skips it when optional dependencies are omitted. Reinstall without ' +
      '--omit=optional / --no-optional: npm install -g @jlongo78/agent-spaces'
    );
  }
  return (
    `No terminal client binary for ${platform}-${arch}. ` +
    'Supported: ' + Object.keys(TARGETS).join(', ')
  );
}

/**
 * A binary built from this repo by `cargo build`, or null.
 *
 * Only ever finds something in a checkout: an installed package has no `tui/`
 * beside `bin/`, so this costs one `existsSync` that fails and nothing else.
 */
function builtTuiBinary(platform = process.platform, root = path.join(__dirname, '..')) {
  const name = binaryFileName(platform);
  for (const profile of ['release', 'debug']) {
    const candidate = path.join(root, 'tui', 'target', profile, name);
    try {
      if (fs.existsSync(candidate)) return candidate;
    } catch {
      // An unreadable path is simply not a candidate.
    }
  }
  return null;
}

/**
 * Absolute path to the platform binary, or null when this platform has no
 * package, the optional dependency was skipped, or the file is missing.
 * Never throws — the caller falls back to the browser flow.
 *
 * `resolve` is injectable (defaults to `require.resolve`) so tests can drive
 * the "package resolves but the binary file is missing" branch without
 * mocking Node's module resolution machinery. `root` is injectable for the
 * same reason: without it a test cannot tell "found nothing" from "found this
 * repo's own build", since the suite runs inside a checkout that has one.
 */
function resolveTuiBinary(
  platform = process.platform,
  arch = process.arch,
  resolve = require.resolve,
  root = path.join(__dirname, '..')
) {
  const pkg = platformPackageName(platform, arch);
  if (pkg) {
    try {
      const entry = resolve(`${pkg}/package.json`);
      const candidate = path.join(path.dirname(entry), 'bin', binaryFileName(platform));
      if (fs.existsSync(candidate)) return candidate;
    } catch {
      // Not installed — fall through to a locally built one.
    }
  }

  // A checkout of this repo, before any per-platform package is published or
  // installed. Without this, `spaces` in a development tree could not find the
  // binary sitting two directories away that the developer had just built: it
  // reported the platform as unsupported and silently opened a browser.
  //
  // Last, not first, so an installed package keeps winning wherever there is
  // one — this changes nothing for anyone who did not build from source.
  return builtTuiBinary(platform, root);
}

/**
 * Splits `--token <value>` out of `argv`.
 *
 * The terminal client prefers `SPACES_TOKEN` from its environment over
 * `--token`, and the caller hands the token over that way instead: an
 * argv entry ends up on the child's command line, which is world-readable
 * via `/proc/<pid>/cmdline` on Linux and Task Manager / WMI on Windows.
 * The last `--token` wins, matching how the client's own parser behaves.
 * A trailing `--token` with no value is left in `rest` untouched so the
 * client produces its own usage error rather than this silently eating it.
 */
function splitToken(argv) {
  const rest = [];
  let token = null;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--token' && i + 1 < argv.length) {
      token = argv[++i];
      continue;
    }
    rest.push(argv[i]);
  }
  return { token, rest };
}

/** Server flags that take no value, as `spaces.js` parses them. */
const SERVER_SWITCHES = new Set(['--desktop', '--no-multiuser', '--multiuser', '--experimental']);

/**
 * Splits the flags meant for the server out of `spaces tui`'s argv.
 *
 * The client accepts only `--url` and `--token` and rejects anything else,
 * so `spaces tui --desktop` or `--port 4000` used to die with
 * "unrecognized argument" — and the background server, started with
 * nothing but `--port`, never saw them either. A valueless trailing
 * `--port` stays with the client so it is reported rather than eaten.
 */
function splitServerFlags(argv) {
  const serverArgs = [];
  const clientArgs = [];
  for (let i = 0; i < argv.length; i++) {
    if (SERVER_SWITCHES.has(argv[i])) {
      serverArgs.push(argv[i]);
    } else if (argv[i] === '--port' && i + 1 < argv.length) {
      serverArgs.push(argv[i], argv[++i]);
    } else {
      clientArgs.push(argv[i]);
    }
  }
  return { serverArgs, clientArgs };
}

/**
 * Whether the local server will run in multiuser mode — the same
 * precedence `startServer()` applies: explicit flag, then
 * `SPACES_MULTIUSER`, then the saved config.
 *
 * The launcher asks before starting anything because multiuser mode
 * refuses `/proto` outright: starting one for the terminal client only
 * strands a server the client can never use.
 */
function resolveMultiuser({ flag, env = {}, saved = {} }) {
  if (flag !== undefined) return flag;
  return env.SPACES_MULTIUSER === '1' || saved.multiuser === true;
}

/**
 * The argv to hand the terminal client: whatever the user passed, plus
 * `--url ws://127.0.0.1:<port>` when they did not name a URL themselves.
 *
 * `--url` is mandatory in the client (no default), and both `spaces tui`
 * and bare `spaces` used to spawn it with nothing at all — so the moment
 * the platform binary is installed, the product's primary command printed
 * a usage error and exited 2 instead of starting.
 */
function buildTuiArgs(argv, port) {
  if (argv.includes('--url')) return argv.slice();
  return ['--url', `ws://127.0.0.1:${port}`, ...argv];
}

/** How long a single connection attempt may take before it counts as a miss. */
const PORT_PROBE_TIMEOUT_MS = 1000;

/** Gap between retries while waiting for a port to come up. */
const PORT_PROBE_RETRY_MS = 250;

/**
 * True once something accepts a TCP connection on `127.0.0.1:<port>`,
 * retrying for up to `waitMs` (`0` — the default — makes a single attempt).
 *
 * This asks "is something listening?", which is not the question a pid
 * lookup answers. The launcher previously used `findPidsOnPort`, which
 * shells out to `lsof`/`fuser` on Unix — absent from most slim Linux
 * images, where it silently returns an empty list. Every `spaces`
 * invocation then believed nothing was listening, spawned another detached
 * server, polled the same broken probe for the full startup timeout, and
 * finally reported that the server had not come up while the one it just
 * started ran on, stranded. `stopServer` tolerates that degradation
 * because it genuinely needs pids and has nothing to do without them; the
 * launcher does not, so it connects instead.
 *
 * Implemented as a child process because the launcher is synchronous
 * (it hands the terminal to `spawnSync`) and `net.connect` is not. One
 * child covers the whole retry window rather than one per attempt.
 *
 * `port` can come from a hand-edited `server.json`, so it is validated
 * against a full-string digit match — *not* `Number.parseInt`, which
 * parses a leading digit prefix and silently discards the rest.
 * `parseInt('3457; rm -rf /')` is `3457`, so the earlier guard coerced
 * where it claimed to reject and went on to probe port 3457; the same
 * held for `'3457.9'` and for an array whose first element is numeric.
 * A malformed value is now genuinely refused before anything is spawned.
 *
 * Values reach the child through `argv`, never through the script text
 * (`PROBE_SCRIPT` is a constant with no interpolation at all). That is
 * belt and braces on top of the validation above: it closes the whole
 * injection class by construction rather than leaving it contingent on
 * the guard staying perfect through future edits.
 */
const PROBE_SCRIPT = `
  const net = require('net');
  const [port, wait, connectTimeout, retryDelay] = process.argv.slice(1).map(Number);
  const deadline = Date.now() + wait;
  const attempt = () => {
    const socket = net.connect({ host: '127.0.0.1', port });
    const miss = () => {
      socket.destroy();
      if (Date.now() < deadline) setTimeout(attempt, retryDelay);
      else process.exit(1);
    };
    socket.setTimeout(connectTimeout, miss);
    socket.on('error', miss);
    socket.on('connect', () => { socket.destroy(); process.exit(0); });
  };
  attempt();
`;

function portIsListening(port, waitMs = 0) {
  // Full-string match: no prefix parsing, no trailing garbage, no floats,
  // no non-strings coerced into one.
  const raw = typeof port === 'number' ? String(port) : port;
  if (typeof raw !== 'string' || !/^\d+$/.test(raw)) return false;
  const p = Number(raw);
  if (p < 1 || p > 65535) return false;

  // `waitMs` is internal (a module constant at every call site), so it is
  // clamped rather than refused.
  const wait = Number.isFinite(Number(waitMs)) ? Math.max(0, Math.floor(Number(waitMs))) : 0;

  const { spawnSync } = require('child_process');
  const r = spawnSync(
    process.execPath,
    [
      '-e',
      PROBE_SCRIPT,
      String(p),
      String(wait),
      String(PORT_PROBE_TIMEOUT_MS),
      String(PORT_PROBE_RETRY_MS),
    ],
    {
      stdio: 'ignore',
      timeout: wait + PORT_PROBE_TIMEOUT_MS + 5000,
    },
  );
  return r.status === 0;
}

/**
 * Waits for a just-started background server: `'up'` once `probe()` sees the
 * port answer, `'exited'` as soon as `alive()` reports the process gone, or
 * `'timeout'`. `probe` should itself block for a short while (the launcher
 * passes `portIsListening(port, 1000)`), which paces the loop.
 *
 * Before this the launcher only probed the port, so a server that died on
 * startup — no build, a bad config — was waited on for the full timeout and
 * then reported as "did not come up", with its actual error thrown away.
 */
function waitForServer({ timeoutMs, probe, alive, clock = Date.now }) {
  const deadline = clock() + timeoutMs;
  while (clock() < deadline) {
    if (probe()) return 'up';
    if (!alive()) return 'exited';
  }
  return 'timeout';
}

/** The last `n` non-blank lines of `text`. */
function tailLines(text, n) {
  return String(text).split(/\r?\n/).filter((l) => l.trim()).slice(-n);
}

/**
 * Runs the terminal client synchronously, reporting a spawn failure
 * instead of swallowing it.
 *
 * `spawnSync().error` covers `ENOENT` (binary missing) and — the case that
 * actually happens in this distribution mechanism — `EACCES`, a binary
 * present but not executable, which is exactly what
 * `actions/upload-artifact` produces since it does not preserve the
 * executable bit. Both leave `status === null`, so a bare
 * `process.exit(r.status === null ? 1 : r.status)` exited 1 with nothing
 * printed at all.
 */
function runTuiBinary(bin, argv, options = {}) {
  const { spawnSync } = require('child_process');
  const r = spawnSync(bin, argv, { stdio: 'inherit', ...options });
  if (r.error) {
    console.error(`  Could not run the terminal client at ${bin}: ${r.error.message}`);
    if (r.error.code === 'EACCES') {
      console.error('  The binary is present but not executable (chmod +x it, or reinstall).');
    }
  }
  return r.status === null ? 1 : r.status;
}

module.exports = {
  builtTuiBinary,
  platformPackageName,
  binaryFileName,
  noBinaryMessage,
  resolveTuiBinary,
  splitToken,
  splitServerFlags,
  resolveMultiuser,
  waitForServer,
  tailLines,
  buildTuiArgs,
  portIsListening,
  runTuiBinary,
  TARGETS,
};

if (require.main === module) {
  const bin = resolveTuiBinary();
  if (!bin) {
    console.error(noBinaryMessage());
    process.exit(1);
  }
  process.exit(runTuiBinary(bin, process.argv.slice(2)));
}
