#!/usr/bin/env node

// Re-exec with higher heap if needed (embedding models + LanceDB leak memory over time)
if (!process.execArgv.some(a => a.includes('max-old-space-size')) && !process.env.__SPACES_HEAP) {
  process.env.__SPACES_HEAP = '1';
  require('child_process').spawn(
    process.execPath,
    ['--max-old-space-size=8192', ...process.argv.slice(1)],
    { stdio: 'inherit', env: process.env }
  ).on('exit', (code) => process.exit(code ?? 1));
  // Stop the outer process from continuing
  return;
}

const net = require('net');
const http = require('http');
const https = require('https');
const { execFileSync, spawn } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');
const crypto = require('crypto');

// Load .env.local into process.env so Next.js API routes see the keys
const _envLocalPath = path.join(__dirname, '..', '.env.local');
try {
  if (fs.existsSync(_envLocalPath)) {
    for (const line of fs.readFileSync(_envLocalPath, 'utf-8').split('\n')) {
      const m = line.match(/^([A-Z_][A-Z0-9_]*)=(.+)$/);
      if (m && !process.env[m[1]]) process.env[m[1]] = m[2].trim();
    }
  }
} catch {}

// terminal-server is loaded lazily (after SPACES_MULTIUSER is set in process.env)

// ─── Memory monitoring (only when SPACES_DEBUG) ──
let _lastHeapMB = 0;
function logMemory(label) {
  if (!process.env.SPACES_DEBUG) return;
  const mem = process.memoryUsage();
  const heapMB = Math.round(mem.heapUsed / 1048576);
  const rssMB = Math.round(mem.rss / 1048576);
  const extMB = Math.round((mem.external || 0) / 1048576);
  const abMB = Math.round((mem.arrayBuffers || 0) / 1048576);
  const delta = heapMB - _lastHeapMB;
  if (label || Math.abs(delta) > 20) {
    console.log(`[Memory] ${label || 'periodic'}: heap=${heapMB}MB rss=${rssMB}MB external=${extMB}MB arrayBuffers=${abMB}MB ${delta > 0 ? '+' : ''}${delta}MB`);
  }
  _lastHeapMB = heapMB;
}
setInterval(() => logMemory(), 60000);

const SPACES_DIR = path.join(os.homedir(), '.spaces');
const CONFIG_PATH = path.join(SPACES_DIR, 'server.json');
const SESSION_SECRET_PATH = path.join(SPACES_DIR, 'session_secret');
const INTERNAL_TOKEN_PATH = path.join(SPACES_DIR, 'internal_token');
let NEXT_INTERNAL_PORT = 3400;
const projectDir = path.join(__dirname, '..');
const MANAGED_PACKAGES = path.join(SPACES_DIR, 'packages');
const MANAGED_NODE_MODULES = path.join(MANAGED_PACKAGES, 'node_modules');

// ─── Terminal client constants (see "Terminal client launch" below) ──
// Declared up here, not next to the functions that use them: the `spaces
// tui` dispatch below calls `launchTui` while this module is still
// evaluating, and a `const` declared further down would still be in its
// temporal dead zone by then.

/** How long to wait for a freshly-started background server to bind. */
const SERVER_START_TIMEOUT_MS = 30000;

/**
 * A terminal client that exits non-zero this fast never reached a usable
 * session (a usage error, a failed connect) — as opposed to one the user
 * quit after actually working in it.
 */
const TUI_LAUNCH_FAILURE_MS = 2000;

// ─── CLI arg parsing ──────────────────────────────────────
const args = process.argv.slice(2);
const cliFlags = {};
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--setup') { cliFlags.setup = true; }
  else if (args[i] === '--port' && args[i + 1]) { cliFlags.port = parseInt(args[++i], 10); }
  else if (args[i] === '--multiuser') { cliFlags.multiuser = true; }
  else if (args[i] === '--no-multiuser' || args[i] === '--desktop') { cliFlags.multiuser = false; }
  else if (args[i] === '--federate' && args[i + 1]) { cliFlags.federate = args[++i]; }
  else if (args[i] === '--federation-port' && args[i + 1]) { cliFlags.federationPort = parseInt(args[++i], 10); }
  else if (args[i] === '--federation-public-interface') { cliFlags.federationAllowPublic = true; }
  else if (args[i] === '--base-path' && args[i + 1]) { cliFlags.basePath = args[++i]; }
  else if (args[i] === '--remote') {
    // Refused here, before anything else runs: without a destination the
    // flag used to fall through to starting a server in the foreground, which
    // first stops whatever server is already running on the port.
    if (!args[i + 1] || args[i + 1].startsWith('-')) {
      console.error('  --remote needs a destination, e.g. spaces --remote you@host');
      process.exit(2);
    }
    cliFlags.remote = args[++i];
  }
  else if (args[i] === '--help' || args[i] === '-h') { cliFlags.help = true; }
  else if (args[i] === '--version' || args[i] === '-v') { cliFlags.version = true; }
  else if (args[i] === '--experimental') { cliFlags.experimental = true; }
  else if (args[i] === 'whoami') { cliFlags.whoami = true; }
}

// A flag nothing recognises is a typo, not a request to start a server.
{
  const unknown = require('./lib/cli-flags').findUnknownFlag(args);
  if (unknown) {
    console.error(`  Unknown option: ${unknown}`);
    console.error('  Run "spaces --help" for usage');
    process.exit(2);
  }
}

if (cliFlags.version) {
  console.log(require('../package.json').version);
  process.exit(0);
}

// Multiuser mode and federation only run with this (see lib/experimental.js).
// The CLI flag or SPACES_EXPERIMENTAL, never a saved setting.
const EXPERIMENTAL = require('./lib/experimental').experimentalEnabled({
  flag: cliFlags.experimental,
  env: process.env,
});
// The terminal server and the web app read the environment, not this
// process's flags: say so there too, or the flag alone would start
// multiuser here while they still behaved as if it were off.
if (EXPERIMENTAL) process.env.SPACES_EXPERIMENTAL = '1';

// Prints what another machine needs to pair with this one, as one line to
// paste. One line rather than three fields because a base64 key is not
// something anyone should be retyping.
if (cliFlags.whoami) {
  const { loadOrCreateIdentity } = require('./lib/network/identity');
  const identity = loadOrCreateIdentity({ dir: SPACES_DIR });

  let saved = {};
  try { saved = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8')); } catch { /* none yet */ }

  // The **federation** port, not the web port. These are different listeners
  // on different interfaces, and printing the web one produced a line that
  // dialled a loopback-only port serving no such path — with a connection
  // refused far from anything that named the cause.
  const port = cliFlags.federationPort || saved.federationPort || 3479;

  // The address federation is actually served on, resolved the same way the
  // listener resolves it. A hostname is only right when the other machine can
  // resolve it, which on a tailnet without MagicDNS it cannot — and the
  // failure looks like a bug in pairing rather than in DNS.
  const iface = cliFlags.federate || saved.federate;
  let host = null;
  if (iface) {
    try {
      host = require('./lib/network/binding').resolveBindAddress({
        iface,
        allowPublic: true, // printing an address is not binding one
      }).address;
    } catch { /* fall through to the hostname */ }
  }

  console.log('');
  console.log(`  ${identity.nodeName}  (${identity.nodeId})`);
  console.log('');
  if (host) {
    console.log('  On the other machine, open the palette, choose Machines,');
    console.log('  then "Pair a new machine" and paste this line:');
    console.log('');
    console.log(`  http://${host}:${port} ${identity.nodeId} ${identity.publicKey}`);
  } else {
    console.log('  This machine is not configured to serve federation, so there is no');
    console.log('  address to publish. Start it with --federate <interface|address>,');
    console.log('  or run: spaces whoami --federate <interface|address>');
    console.log('');
    console.log(`  id:  ${identity.nodeId}`);
    console.log(`  key: ${identity.publicKey}`);
  }
  console.log('');
  console.log('  Pairing must be done in BOTH directions — the handshake is mutual.');
  console.log('');
  for (const line of require('./lib/experimental').FEDERATION_WHOAMI_NOTE) console.log(line);
  console.log('');
  process.exit(0);
}

if (cliFlags.help) {
  console.log(`
  Spaces - Agent Workspace Manager

  Usage:
    spaces                       Start the server (desktop mode by default)
    spaces tui                   Launch the terminal client
    spaces --remote you@host     The terminal client for Spaces on another machine,
                                 over SSH (Spaces must be installed there)
    spaces start                 Start the server in the background, if it is not running
    spaces open                  Open the web app in your browser, signed in
    spaces signout               Sign out every browser and terminal client
    spaces stop                  Stop running server
    spaces upgrade               Upgrade Spaces to the latest version
    spaces uninstall             Uninstall Spaces
    spaces --setup               Interactive first-time setup wizard
    spaces --port 3457           Override port
    spaces --desktop             Force desktop mode, overriding a saved multiuser setting
    spaces --experimental        Allow the experimental features below for this run
                                 (or set SPACES_EXPERIMENTAL=1). Not saved anywhere.
    spaces --multiuser           [experimental] Multiuser mode (LAN-reachable, login
                                 required). Not safe yet: any signed-in user can gain
                                 control of the server account. Needs --experimental.
    spaces --federate <iface>    [experimental] Serve this machine to paired machines on
                                 one interface (an overlay address such as Tailscale is
                                 strongly preferred). A paired machine can act as you and
                                 traffic is unencrypted. Needs --experimental.
    spaces --federation-port <n> Port for the federation listener (default 3479)
    spaces --federation-public-interface
                                 Permit --federate on a public address. Read the docs first:
                                 the channel it serves can start processes on this machine.
    spaces --base-path /spaces   Set base path for reverse proxy
    spaces service install       Install as OS service (systemd/launchd/Task Scheduler)
    spaces service uninstall     Remove OS service
    spaces service start         Start the service
    spaces service stop          Stop the service
    spaces service status        Show service status
    spaces service logs          Tail service logs
    spaces reset-totp <user>     Reset TOTP/2FA for a user
    spaces guide                 Print the agent guide (how coding agents use Spaces)
    spaces --version, -v         Print the installed version
    spaces --help                Show this help

  Coding agents: run "spaces guide", or fetch /agent-guide.md and /llms.txt
  from the server for a machine-readable overview.
`);
  process.exit(0);
}

// ─── Stop command ─────────────────────────────────────────
const subcommand = args[0];
// `--remote` means "the terminal client, over there". With any other command
// it would be silently ignored and the command would run here instead.
if (cliFlags.remote && subcommand && subcommand !== 'tui' && !subcommand.startsWith('-')) {
  console.error(`  --remote works with "spaces" and "spaces tui", not "spaces ${subcommand}".`);
  console.error(`  To run that on the other machine: ssh ${cliFlags.remote} spaces ${subcommand}`);
  process.exit(2);
}
if (subcommand === 'stop') {
  stopServer(cliFlags.port);
  process.exit(0);
}

// ─── Agent guide ──────────────────────────────────────────
if (subcommand === 'guide') {
  const { resolveAgentGuide } = require('./lib/agent-guide');
  const guide = resolveAgentGuide(path.join(__dirname, '..'));
  if (!guide) {
    console.error('  The agent guide is missing from this install.');
    process.exit(1);
  }
  process.stdout.write(fs.readFileSync(guide, 'utf-8'));
  process.exit(0);
}

// ─── Route uninstall/upgrade to spaces-install.js ────
if (subcommand === 'uninstall' || subcommand === 'upgrade') {
  // Re-exec with spaces-install.js, passing through all args
  const installScript = path.join(__dirname, 'spaces-install.js');
  const { status } = require('child_process').spawnSync(
    process.execPath, [installScript, ...args],
    { stdio: 'inherit', env: process.env }
  );
  process.exit(status || 0);
}

// ─── Route reset-totp to spaces-reset-totp.js ───────
if (subcommand === 'reset-totp') {
  const resetScript = path.join(__dirname, 'spaces-reset-totp.js');
  const { status } = require('child_process').spawnSync(
    process.execPath, [resetScript, ...args.slice(1)],
    { stdio: 'inherit', env: process.env }
  );
  process.exit(status || 0);
}

// ─── Route service commands to spaces-service.js ─────
if (subcommand === 'service') {
  const serviceScript = path.join(__dirname, 'spaces-service.js');
  const { status } = require('child_process').spawnSync(
    process.execPath, [serviceScript, ...args.slice(1)],
    { stdio: 'inherit', env: process.env }
  );
  process.exit(status || 0);
}

// ─── Route tui to the platform terminal-client binary ─────
if (subcommand === 'tui' || cliFlags.remote) {
  // Explicit `spaces tui` never silently opens a browser: the user asked
  // for the terminal client by name, so a failure is reported as one. Bare
  // `spaces --remote host` asks for the same thing.
  launchTui(subcommand === 'tui' ? args.slice(1) : args, { fallbackToBrowser: false });
}

// ─── Start in the background ──────────────────────────────
// What `--remote` runs on the other machine; useful on its own too.
if (subcommand === 'start') {
  startDetached(args.slice(1));
}

// ─── Sign out everywhere ──────────────────────────────────
if (subcommand === 'signout') {
  signOutEverywhere();
}

// ─── Open the web app, signed in / setup wizard / server ──
if (subcommand === 'open') {
  // Exits by itself: at once, or on Linux once the timer that removes the
  // sign-in redirect page fires. Part of this chain so it no longer falls
  // through to "Unknown command" while that timer is pending.
  openSignedIn();
} else if (cliFlags.setup) {
  require('./spaces-setup');
  // spaces-setup handles its own process.exit
} else if (subcommand && !subcommand.startsWith('-')) {
  console.error(`  Unknown command: ${subcommand}`);
  console.error(`  Run "spaces --help" for usage`);
  process.exit(1);
} else if (!subcommand && process.stdout.isTTY) {
  // Bare `spaces` in a TTY prefers the terminal client when the platform
  // binary is available; otherwise fall back to today's browser flow —
  // and fall back to it too if the client can't get off the ground, since
  // the user asked for "spaces", not specifically for a TUI.
  launchTui([], { fallbackToBrowser: true });
} else {
  startServer();
}

// ─── Terminal client launch ─────────────────────────────────
// The client is a peer that talks to a running server over the documented
// WebSocket protocol; it does not embed one. `startServer()` runs the
// server *in this process*, and `spawnSync`-ing a full-screen client would
// block that process's event loop — so when nothing is listening yet, the
// server is started as a detached child and this process waits for the
// port before handing the terminal over.
//
// Only function declarations live below the dispatch above (they hoist);
// the two constants this section needs are declared with it, since
// `spaces tui` calls into here while this file is still evaluating.

/** `server.json`, or `{}` when it is missing or unreadable. */
function readSavedConfig() {
  if (!fs.existsSync(CONFIG_PATH)) return {};
  try { return JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8')); } catch { return {}; }
}

/**
 * Same precedence `startServer()` uses: CLI > env > saved config > default.
 *
 * The default is only a default for the first account on a machine. Each
 * account runs its own server under its own secret, so two people using
 * `--remote` against the same host each need a port of their own: when the
 * default belongs to another account's server, this account claims a free
 * one and saves it, so it is the same on every later start and connect.
 */
function resolvePort() {
  const { resolveAccountPort, DEFAULT_PORT } = require('./lib/ports');
  const { portIsListening } = require('./spaces-tui');
  const savedConfig = readSavedConfig();
  const { port, claimed } = resolveAccountPort({
    chosen: cliFlags.port || parseInt(process.env.SPACES_PORT || '', 10) || savedConfig.port,
    isMine: listenerIsMine,
    isListening: (p) => portIsListening(p),
    save: (p) => {
      const config = readSavedConfig();
      config.port = p;
      fs.mkdirSync(SPACES_DIR, { recursive: true, mode: 0o700 });
      fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2) + '\n', { mode: 0o600 });
    },
  });
  if (claimed) {
    console.error(`  Port ${DEFAULT_PORT} belongs to another account's Spaces; this account uses ${port}.`);
  }
  return port;
}

/**
 * Starts a detached server on `port` and waits for it to bind. Returns
 * true once something is listening there. `serverArgs` are the server
 * flags the user gave `spaces tui` (`--desktop`, `--multiuser`, ...).
 *
 * `SPACES_SERVICE=1` suppresses the child's browser-open (the whole point
 * here is that the terminal, not a browser, is the UI). `__SPACES_HEAP` is
 * cleared so the child performs its own heap re-exec rather than inheriting
 * this process's "already re-execed" marker and running with the default
 * heap size.
 *
 * Readiness is a real TCP connect (`portIsListening`), not a pid lookup —
 * see that function for why the pid lookup is the wrong question here.
 */
function startBackgroundServer(port, serverArgs = []) {
  const { portIsListening, waitForServer, tailLines } = require('./spaces-tui');
  console.log(`  Starting the Spaces server on port ${port} in the background...`);
  const env = { ...process.env, SPACES_SERVICE: '1' };
  delete env.__SPACES_HEAP;
  // Its output goes to a log rather than nowhere, so a server that fails to
  // start can say why.
  fs.mkdirSync(SPACES_DIR, { recursive: true });
  const logPath = path.join(SPACES_DIR, 'server.log');
  const log = fs.openSync(logPath, 'w');
  // `--port` last so the resolved port wins over any the user passed.
  const child = spawn(process.execPath, [__filename, ...serverArgs, '--port', String(port)], {
    detached: true,
    stdio: ['ignore', log, log],
    env,
  });
  fs.closeSync(log);
  child.unref();

  const alive = () => {
    try { process.kill(child.pid, 0); return true; } catch (e) { return e.code === 'EPERM'; }
  };
  const outcome = waitForServer({
    timeoutMs: SERVER_START_TIMEOUT_MS,
    probe: () => portIsListening(port, 1000),
    alive,
  });
  if (outcome === 'up') return true;

  console.error(outcome === 'exited'
    ? `  The Spaces server exited before it came up on port ${port}:`
    : `  The Spaces server did not come up on port ${port} within ${SERVER_START_TIMEOUT_MS / 1000}s:`);
  let output = '';
  try { output = fs.readFileSync(logPath, 'utf-8'); } catch { /* nothing logged */ }
  for (const line of tailLines(output, 12)) console.error(`    ${line}`);
  console.error(`  Full output: ${logPath}`);
  return false;
}

/**
 * Resolves the platform binary, makes sure there is a server to talk to,
 * and hands the terminal over to the client.
 *
 * Never returns: every path either execs into the client and exits with
 * its status, or (when `fallbackToBrowser`) hands off to `startServer()`.
 */
function launchTui(tuiArgs, { fallbackToBrowser }) {
  const {
    resolveTuiBinary,
    noBinaryMessage,
    splitToken,
    splitServerFlags,
    resolveMultiuser,
    buildTuiArgs,
    portIsListening,
    runTuiBinary,
  } = require('./spaces-tui');

  const { splitRemote } = require('./lib/remote');
  const { remote, rest: localArgs } = splitRemote(tuiArgs);
  if (localArgs.includes('--remote')) {
    console.error('  --remote needs a destination, e.g. spaces --remote you@host');
    process.exit(2);
  }
  if (remote !== null) {
    launchRemote(remote, localArgs);
    return;
  }

  const tuiBin = resolveTuiBinary();
  if (!tuiBin) {
    if (fallbackToBrowser) {
      console.log('  Terminal client unavailable for this platform — opening the browser instead.');
      startServer();
      return;
    }
    console.error('  ' + noBinaryMessage());
    process.exit(1);
  }

  // The token goes through the environment, not argv: this process's own
  // command line already carries whatever the user typed (twice, thanks to
  // the heap re-exec above), and there is no reason to put it on a third.
  const { serverArgs, clientArgs } = splitServerFlags(tuiArgs);
  const { token, rest } = splitToken(clientArgs);
  const env = { ...process.env };
  if (token) env.SPACES_TOKEN = token;

  const port = resolvePort();
  // An explicit `--url` means the user is pointing at some other node;
  // starting a local server would be both useless and surprising.
  const local = !rest.includes('--url');
  const multiuser = resolveMultiuser({
    flag: cliFlags.multiuser,
    env: process.env,
    saved: readSavedConfig(),
  });
  if (local && multiuser) {
    // Multiuser mode refuses /proto, so a server started now could never
    // serve this client. Say why instead of stranding one in the background.
    console.error('  The terminal client needs desktop mode, but multiuser mode is on');
    console.error('  (SPACES_MULTIUSER=1 or "multiuser": true in ~/.spaces/server.json).');
    console.error('  Run "spaces tui --desktop" to use the terminal client anyway.');
    if (fallbackToBrowser) {
      console.log('  Opening the browser instead.');
      startServer();
      return;
    }
    process.exit(1);
  }
  if (local && !portIsListening(port)) {
    if (!startBackgroundServer(port, serverArgs)) {
      // No browser fallback: it would start the same server in the
      // foreground and fail the same way, repeating the error above.
      process.exit(1);
    }
    console.log('  Server running in the background — "spaces stop" shuts it down.');
  }
  // The server only answers its owner (see bin/lib/desktop-auth.js). The
  // client gets a signed session token, not the secret it was signed with.
  if (local && !token) {
    if (!listenerIsMine(port)) refuseForeignListener(port);
    const session = desktopSessionToken();
    if (!session) {
      console.error(`  Could not read the Spaces secret in ${SPACES_DIR}; is the server running as you?`);
      process.exit(1);
    }
    env.SPACES_TOKEN = session;
  }

  const startedAt = Date.now();
  const status = runTuiBinary(tuiBin, buildTuiArgs(rest, port), { env });
  if (status !== 0 && fallbackToBrowser && Date.now() - startedAt < TUI_LAUNCH_FAILURE_MS) {
    console.log('  The terminal client could not start — opening the browser instead.');
    startServer();
    return;
  }
  process.exit(status);
}

/**
 * `spaces start`: a background server on this machine if none is running,
 * then `SPACES_PORT=<port>` on its own line so `spaces --remote` can find it.
 *
 * In multiuser mode it prints `SPACES_MULTIUSER=1` instead of a port: that
 * server refuses the terminal client, and saying so beats a connection
 * that fails for no visible reason on the other end of a tunnel.
 */
function startDetached(startArgs) {
  const { portIsListening, splitServerFlags, resolveMultiuser } = require('./spaces-tui');
  const { serverArgs } = splitServerFlags(startArgs);
  const port = resolvePort();
  const multiuser = resolveMultiuser({ flag: cliFlags.multiuser, env: process.env, saved: readSavedConfig() });

  if (!portIsListening(port)) {
    // Say it here rather than leaving a background server to refuse into its log.
    const gate = require('./lib/experimental');
    const req = gate.multiuserRequest({ flag: cliFlags.multiuser, env: process.env, saved: readSavedConfig() });
    const verdict = gate.gateMultiuser({ multiuser: req.multiuser, source: req.source, experimental: EXPERIMENTAL });
    if (!verdict.allowed) {
      for (const line of verdict.lines) console.error(line);
      process.exit(1);
    }
  }

  if (portIsListening(port)) {
    console.log(`  Spaces is already running on port ${port}.`);
  } else {
    if (!startBackgroundServer(port, serverArgs)) process.exit(1);
    console.log('  Server running in the background — "spaces stop" shuts it down.');
  }
  console.log(multiuser ? 'SPACES_MULTIUSER=1' : `SPACES_PORT=${port}`);
  // Only on request: `--remote` asks, over its SSH channel. A session token,
  // never the secret, and never printed by a plain `spaces start` where it
  // could land in a log or scrollback.
  if (!multiuser && startArgs.includes('--print-token')) {
    if (!listenerIsMine(port)) refuseForeignListener(port);
    const session = desktopSessionToken();
    if (session) console.log(`SPACES_TOKEN=${session}`);
  }
  process.exit(0);
}

/** A signed desktop session token for this machine's server, or null. */
function desktopSessionToken() {
  const desktopAuth = require('./lib/desktop-auth');
  const secret = desktopAuth.readSecret(SPACES_DIR);
  if (!secret) return null;
  try {
    return desktopAuth.signDesktop(secret, 'session', desktopAuth.SESSION_TTL_S);
  } catch {
    return null;
  }
}

/** Opens `url` in the default browser; false when that is not possible. */
function openInBrowser(url) {
  try {
    if (process.platform === 'win32') {
      execFileSync('cmd', ['/c', 'start', '', url], { stdio: 'ignore' });
    } else if (process.platform === 'darwin') {
      execFileSync('open', [url], { stdio: 'ignore' });
    } else {
      execFileSync('xdg-open', [url], { stdio: 'ignore' });
    }
    return true;
  } catch {
    return false;
  }
}

/**
 * Opens a sign-in link. On Linux any account can read every process's
 * command line, so the link is not passed to the browser as an argument
 * there — another account could race it to the server. Instead an
 * owner-only page that redirects to it is written to ~/.spaces and opened,
 * and removed again a few seconds later.
 */
function openSignInLink(url) {
  if (process.platform !== 'linux') return openInBrowser(url);
  const crypto = require('crypto');
  const page = path.join(SPACES_DIR, `open-${crypto.randomBytes(8).toString('hex')}.html`);
  const safe = url.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');
  try {
    fs.mkdirSync(SPACES_DIR, { recursive: true, mode: 0o700 });
    fs.writeFileSync(page,
      `<!doctype html><meta http-equiv="refresh" content="0;url=${safe}"><a href="${safe}">Open Spaces</a>`,
      { mode: 0o600 });
  } catch {
    return false;
  }
  const opened = openInBrowser(`file://${page}`);
  const timer = setTimeout(() => { try { fs.unlinkSync(page); } catch {} }, 15000);
  if (!opened) { clearTimeout(timer); try { fs.unlinkSync(page); } catch {} }
  return opened;
}

/**
 * False when the socket listening on `port` belongs to another account.
 * Checked before handing anything a credential: while Spaces is down, any
 * account can bind its port and wait for the next `spaces tui` to sign in
 * to it. Linux only, from /proc/net/tcp; other systems give no cheap way to
 * ask, and are single-user far more often.
 */
function listenerIsMine(port) {
  if (process.platform !== 'linux' || typeof process.getuid !== 'function') return true;
  const uids = require('./lib/proc-ports').listenerUids(Number(port));
  return uids.every((uid) => uid === process.getuid());
}

function refuseForeignListener(port) {
  console.error(`  Port ${port} is held by a process belonging to another account.`);
  console.error('  Not signing in to it. Stop that process, or run Spaces on another port with --port.');
  process.exit(1);
}

/** The one-time sign-in link for a desktop server on `port`. */
function signInUrl(port, basePath, secret) {
  const { createLoginCodes } = require('./lib/desktop-auth');
  const code = createLoginCodes(() => secret).mint();
  return `http://localhost:${port}${basePath || ''}/auth/desktop?code=${code}`;
}

/**
 * `spaces open`: starts the server if needed and opens the web app in the
 * browser, already signed in through a one-time link.
 */
function openSignedIn() {
  const { portIsListening, resolveMultiuser } = require('./spaces-tui');
  const desktopAuth = require('./lib/desktop-auth');
  const port = resolvePort();
  const saved = readSavedConfig();
  const basePath = cliFlags.basePath || process.env.SPACES_BASE_PATH || saved.basePath || '';
  if (resolveMultiuser({ flag: cliFlags.multiuser, env: process.env, saved })) {
    const url = `http://localhost:${port}${basePath}`;
    if (!openInBrowser(url)) console.log(`  Open ${url} in your browser and sign in.`);
    process.exit(0);
  }
  if (!portIsListening(port) && !startBackgroundServer(port, [])) process.exit(1);
  if (!listenerIsMine(port)) refuseForeignListener(port);
  const secret = desktopAuth.readSecret(SPACES_DIR);
  if (!secret) {
    console.error(`  Could not read the Spaces secret in ${SPACES_DIR}; is the server running as you?`);
    process.exit(1);
  }
  const url = signInUrl(port, basePath, secret);
  if (openSignInLink(url)) {
    console.log('  Opened Spaces in your browser, signed in.');
    // Long enough for the browser to read the redirect page, which is
    // removed when this process's timer fires.
    setTimeout(() => process.exit(0), process.platform === 'linux' ? 15500 : 0);
    return;
  }
  console.log('  Open this link in your browser within two minutes (it works once):');
  console.log(`    ${url}`);
  process.exit(0);
}

/**
 * `spaces signout`: replaces the owner's secret, which invalidates every
 * token ever signed with it — browser sessions, terminal clients, remote
 * sessions — and restarts a running server so it uses the new one.
 */
function signOutEverywhere() {
  const crypto = require('crypto');
  const { portIsListening } = require('./spaces-tui');
  if (process.env.SPACES_INTERNAL_TOKEN) {
    console.error('  SPACES_INTERNAL_TOKEN is set in the environment; change it there instead.');
    process.exit(1);
  }
  fs.mkdirSync(SPACES_DIR, { recursive: true, mode: 0o700 });
  fs.writeFileSync(INTERNAL_TOKEN_PATH, crypto.randomBytes(32).toString('hex'), { mode: 0o600 });
  if (process.platform !== 'win32') fs.chmodSync(INTERNAL_TOKEN_PATH, 0o600);
  const port = resolvePort();
  if (portIsListening(port)) {
    stopServer(port);
    if (!startBackgroundServer(port, [])) process.exit(1);
    console.log('  If Spaces runs as a service, restart the service too, so it picks up the new secret.');
  }
  console.log('  Signed out everywhere: every browser, terminal client and remote session.');
  console.log('  Run "spaces open" or "spaces tui" to sign in again.');
  process.exit(0);
}

/** A TCP port free on this machine's loopback right now, or null. */
function freeLocalPort() {
  const { spawnSync } = require('child_process');
  const r = spawnSync(process.execPath, ['-e', `
    const s = require('net').createServer();
    s.listen(0, '127.0.0.1', () => { process.stdout.write(String(s.address().port)); s.close(); });
  `], { encoding: 'utf-8', timeout: 5000 });
  const port = Number(r.stdout);
  return Number.isInteger(port) && port > 0 ? port : null;
}

/**
 * `spaces --remote <dest>`: starts Spaces on `dest` over SSH, tunnels a
 * local port to it, and runs the terminal client through the tunnel. See
 * bin/lib/remote.js for why it is built on SSH.
 *
 * Never returns.
 */
function launchRemote(dest, tuiArgs) {
  const remote = require('./lib/remote');
  const {
    resolveTuiBinary, noBinaryMessage, splitServerFlags, splitToken,
    buildTuiArgs, portIsListening, waitForServer, tailLines, runTuiBinary,
  } = require('./spaces-tui');
  const { spawnSync } = require('child_process');

  try {
    remote.validateDestination(dest);
  } catch (e) {
    console.error(`  ${e.message}`);
    process.exit(2);
  }
  const { serverArgs, clientArgs } = splitServerFlags(tuiArgs);
  if (serverArgs.length || cliFlags.port) {
    console.error('  --port, --desktop, --multiuser and --experimental describe this machine\'s server, not the remote\'s.');
    console.error(`  The remote uses its own ~/.spaces/server.json; change it there (ssh ${dest}).`);
    process.exit(2);
  }
  if (clientArgs.includes('--url')) {
    console.error('  --remote and --url both say where to connect; use one.');
    process.exit(2);
  }
  const tuiBin = resolveTuiBinary();
  if (!tuiBin) {
    console.error('  ' + noBinaryMessage());
    process.exit(1);
  }

  // 1. Make sure Spaces is running over there, and learn its port.
  console.log(`  Connecting to ${dest}...`);
  const runStart = (loginShell) => spawnSync('ssh', remote.sshStartArgs(dest, { loginShell }), {
    stdio: ['inherit', 'pipe', 'pipe'],
    encoding: 'utf-8',
  });
  let r = runStart(false);
  if (r.error) {
    console.error(r.error.code === 'ENOENT'
      ? '  --remote needs the ssh client, and none was found on the PATH.'
      : `  Could not run ssh: ${r.error.message}`);
    process.exit(1);
  }
  let remotePort = r.status === 0 ? remote.parseStartOutput(r.stdout) : null;
  if (remotePort === null && remote.classifyStartFailure(r.status, r.stderr) === 'missing') {
    r = runStart(true);
    remotePort = r.status === 0 ? remote.parseStartOutput(r.stdout) : null;
  }
  const relay = (text) => {
    for (const line of tailLines(text, 12)) {
      if (!/^SPACES_(PORT|MULTIUSER|TOKEN)=/.test(line)) console.error(`    ${line.trim()}`);
    }
  };
  if (remotePort === null) {
    if (r.status === 0 && /^SPACES_MULTIUSER=1/m.test(r.stdout)) {
      console.error(`  Spaces on ${dest} runs in multiuser mode, which the terminal client cannot use.`);
      console.error('  Set "multiuser": false in ~/.spaces/server.json there and restart it.');
      process.exit(1);
    }
    const kind = remote.classifyStartFailure(r.status, r.stderr);
    if (kind === 'missing') {
      console.error(remote.installHint(dest));
    } else if (kind === 'ssh') {
      console.error(`  Could not connect to ${dest} over SSH:`);
      relay(r.stderr);
    } else {
      console.error(`  Spaces on ${dest} did not start:`);
      relay(`${r.stdout}\n${r.stderr}`);
    }
    process.exit(1);
  }

  // 2. Tunnel a free local port to it.
  const localPort = freeLocalPort();
  if (!localPort) {
    console.error('  Could not find a free local port for the tunnel.');
    process.exit(1);
  }
  fs.mkdirSync(SPACES_DIR, { recursive: true });
  const logPath = path.join(SPACES_DIR, 'remote-tunnel.log');
  const log = fs.openSync(logPath, 'w');
  const tunnel = spawn('ssh', remote.sshTunnelArgs(dest, localPort, remotePort), {
    stdio: ['ignore', log, log],
  });
  fs.closeSync(log);
  const closeTunnel = () => { try { tunnel.kill(); } catch { /* already gone */ } };
  const outcome = waitForServer({
    timeoutMs: 30000,
    probe: () => portIsListening(localPort, 1000),
    alive: () => { try { process.kill(tunnel.pid, 0); return true; } catch (e) { return e.code === 'EPERM'; } },
  });
  if (outcome !== 'up') {
    closeTunnel();
    console.error(`  Could not open the SSH tunnel to ${dest}:`);
    let output = '';
    try { output = fs.readFileSync(logPath, 'utf-8'); } catch { /* nothing logged */ }
    relay(output);
    process.exit(1);
  }

  // 3. The client, through the tunnel. The remote trusts it as local,
  // because over there that is what it is.
  const { token, rest } = splitToken(clientArgs);
  const env = { ...process.env };
  // The remote's own session token, handed back over SSH by
  // `spaces start --print-token`; the remote's secret never leaves it.
  const remoteToken = token || remote.parseStartToken(r.stdout);
  if (!remoteToken) {
    closeTunnel();
    console.error(`  Spaces on ${dest} did not hand back a sign-in token. It may be an older`);
    console.error(`  version; update it there (ssh ${dest} 'npm install -g @jlongo78/agent-spaces').`);
    process.exit(1);
  }
  env.SPACES_TOKEN = remoteToken;
  const status = runTuiBinary(tuiBin, buildTuiArgs(rest, localPort), { env });
  closeTunnel();
  console.log(`  Spaces is still running on ${dest} — "ssh ${dest} spaces stop" shuts it down.`);
  process.exit(status);
}

// ─── TLS certificate ────────────────────────────────────────
// Uses a real cert if configured in server.json (tlsCert, tlsKey, optional
// tlsCa for the full chain), otherwise generates a self-signed cert so
// mobile browsers that force HTTPS-first can still connect.
function ensureTlsCert() {
  // Check for configured real cert in server.json
  try {
    const config = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8'));
    if (config.tlsCert && config.tlsKey) {
      if (fs.existsSync(config.tlsCert) && fs.existsSync(config.tlsKey)) {
        // Read cert + key, and optionally the CA bundle for full chain
        const certPem = fs.readFileSync(config.tlsCert, 'utf-8').trim();
        const caPem = (config.tlsCa && fs.existsSync(config.tlsCa))
          ? fs.readFileSync(config.tlsCa, 'utf-8').trim()
          : null;
        const creds = {
          cert: caPem ? certPem + '\n' + caPem : certPem,
          key: fs.readFileSync(config.tlsKey, 'utf-8'),
        };
        console.log('  Using TLS certificate: ' + config.tlsCert);
        return creds;
      }
      console.log('  Warning: tlsCert/tlsKey configured but files not found, falling back to self-signed');
    }
  } catch {}

  // Fall back to self-signed
  const certPath = path.join(SPACES_DIR, 'tls-cert.pem');
  const keyPath  = path.join(SPACES_DIR, 'tls-key.pem');

  if (fs.existsSync(certPath) && fs.existsSync(keyPath)) {
    try {
      return { cert: fs.readFileSync(certPath), key: fs.readFileSync(keyPath) };
    } catch {}
  }

  try {
    fs.mkdirSync(SPACES_DIR, { recursive: true });
    execFileSync('openssl', [
      'req', '-x509', '-newkey', 'rsa:2048',
      '-keyout', keyPath, '-out', certPath,
      '-sha256', '-days', '365', '-nodes',
      '-subj', '/CN=spaces-local',
      '-addext', 'subjectAltName=DNS:localhost,DNS:*.local,IP:127.0.0.1',
    ], { stdio: 'pipe', timeout: 15000 });
    console.log('  Generated self-signed TLS certificate');
    return { cert: fs.readFileSync(certPath), key: fs.readFileSync(keyPath) };
  } catch (e) {
    console.log(`  Warning: Could not generate TLS cert (${e.message})`);
    console.log('  HTTPS will not be available. Install OpenSSL to enable it.');
    return null;
  }
}

function startServer() {
  // ─── Load saved config ────────────────────────────────────
  let savedConfig = {};
  if (fs.existsSync(CONFIG_PATH)) {
    try { savedConfig = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8')); } catch {}
  }

  // ─── Resolve settings (CLI > env > config > defaults) ─────
  const PORT = cliFlags.port
    || parseInt(process.env.SPACES_PORT || '', 10)
    || savedConfig.port
    || 3457;

  const basePath = cliFlags.basePath
    || process.env.SPACES_BASE_PATH
    || savedConfig.basePath
    || '';

  const allowedOrigins = process.env.SPACES_ALLOWED_ORIGINS
    || savedConfig.allowedOrigins
    || '';

  // Use a unique internal port so multiple instances don't collide
  NEXT_INTERNAL_PORT = PORT - 57; // 3457→3400, 3458→3401, etc.

  // Multiuser is opt-in. Desktop mode is the default for npm installs.
  // Federation is always on — the /api/network/* routes exist regardless of mode.
  // An explicit flag wins over everything, in both directions. Until
  // `--desktop`/`--no-multiuser` existed the setting was one-way: once
  // `multiuser: true` was saved to server.json there was no flag that turned
  // it off, and multiuser mode disables `/proto` — so a saved preference
  // silently made the terminal client unable to connect, with editing the file
  // by hand as the only escape.
  const experimentalGate = require('./lib/experimental');
  const multiuserReq = experimentalGate.multiuserRequest({
    flag: cliFlags.multiuser,
    env: process.env,
    saved: savedConfig,
  });
  const isMultiuser = multiuserReq.multiuser;

  // Multiuser mode is experimental (see lib/experimental.js). Refused before
  // anything else happens — in particular before the server already running
  // on this port is stopped below.
  const multiuserGate = experimentalGate.gateMultiuser({
    multiuser: isMultiuser,
    source: multiuserReq.source,
    experimental: EXPERIMENTAL,
  });
  if (!multiuserGate.allowed) {
    console.error('');
    for (const line of multiuserGate.lines) console.error(line);
    console.error('');
    process.exit(1);
  }

  // ─── Kill any existing server on this port ───────────────────
  stopServer(PORT);

  // Desktop mode is single-user with no login, so it must be local-only —
  // any LAN reach without auth would be a free shell on the user's machine.
  // Multiuser mode binds the wildcard interface; every request must
  // authenticate via the session cookie (or a network bearer token).
  const bindHost = isMultiuser ? '0.0.0.0' : '127.0.0.1';

  console.log('');
  console.log('  Spaces - Agent Workspace Manager');
  console.log('  =================================');
  console.log('');
  console.log(`  Mode: ${isMultiuser ? 'multiuser (LAN-reachable, login required)' : 'desktop (localhost only)'}`);

  const childEnv = { ...process.env };
  if (!isMultiuser) {
    // An explicit --desktop must win everywhere: Next and the terminal
    // server read these themselves, and left set they would run multiuser
    // auth behind a front server running desktop auth.
    for (const key of ['SPACES_MULTIUSER', 'NEXT_PUBLIC_MULTIUSER']) {
      delete childEnv[key];
      delete process.env[key];
    }
  }

  if (isMultiuser) {
    // Ensure session secret + admin DB exist. autoSetup handles both.
    let sessionSecret = process.env.SPACES_SESSION_SECRET || '';
    if (!sessionSecret && fs.existsSync(SESSION_SECRET_PATH)) {
      sessionSecret = fs.readFileSync(SESSION_SECRET_PATH, 'utf-8').trim();
    }
    const adminDbPath = path.join(SPACES_DIR, 'admin.db');
    if (!fs.existsSync(adminDbPath) || !sessionSecret) {
      const { autoSetup } = require('./lib/auto-setup');
      const result = autoSetup({ SPACES_DIR, SESSION_SECRET_PATH, INTERNAL_TOKEN_PATH, ADMIN_DB_PATH: adminDbPath, CONFIG_PATH, port: PORT, basePath });
      sessionSecret = result.sessionSecret;
    }
    childEnv.SPACES_SESSION_SECRET = sessionSecret;
    process.env.SPACES_SESSION_SECRET = sessionSecret;
    childEnv.SPACES_MULTIUSER = '1';
    process.env.SPACES_MULTIUSER = '1';
    console.log(`  Admin DB: ${adminDbPath}`);
  }

  // Internal token — the owner's secret. In desktop mode every credential
  // derives from it (see lib/desktop-auth.js); in multiuser mode local hooks,
  // MCP and agent processes authenticate with it. Separate from
  // SPACES_SESSION_SECRET, which signs multiuser logins.
  let internalToken = process.env.SPACES_INTERNAL_TOKEN || '';
  if (!internalToken && fs.existsSync(INTERNAL_TOKEN_PATH)) {
    internalToken = fs.readFileSync(INTERNAL_TOKEN_PATH, 'utf-8').trim();
  }
  if (!internalToken) {
    internalToken = crypto.randomBytes(32).toString('hex');
    if (!fs.existsSync(SPACES_DIR)) fs.mkdirSync(SPACES_DIR, { recursive: true });
    fs.writeFileSync(INTERNAL_TOKEN_PATH, internalToken, { mode: 0o600 });
  }
  childEnv.SPACES_INTERNAL_TOKEN = internalToken;
  process.env.SPACES_INTERNAL_TOKEN = internalToken;

  // The secret is only as good as the permissions on it. It is created
  // owner-only, but a file or directory loosened since (a restore, a copy,
  // a umask) would hand it to every account on the machine. Windows keeps
  // ~/.spaces inside the owner's profile, whose ACL already does this.
  if (process.platform !== 'win32') {
    try {
      const dirStat = fs.statSync(SPACES_DIR);
      const fileStat = fs.statSync(INTERNAL_TOKEN_PATH);
      if (typeof process.getuid === 'function' && fileStat.uid !== process.getuid()) {
        console.error(`  ${INTERNAL_TOKEN_PATH} belongs to another account. Refusing to start.`);
        process.exit(1);
      }
      if (dirStat.mode & 0o077) fs.chmodSync(SPACES_DIR, 0o700);
      if (fileStat.mode & 0o077) fs.chmodSync(INTERNAL_TOKEN_PATH, 0o600);
    } catch { /* created just above with the right modes */ }
  }

  // Desktop-mode authentication: the internal token is the owner's secret.
  const desktopAuth = require('./lib/desktop-auth');
  const loginCodes = desktopAuth.createLoginCodes(() => internalToken);
  const allowedHostList = String(allowedOrigins || '')
    .split(',').map((h) => h.trim()).filter(Boolean);

  if (basePath) {
    childEnv.SPACES_BASE_PATH = basePath;
    console.log(`  Base path: ${basePath}`);
  }
  if (allowedOrigins) {
    childEnv.SPACES_ALLOWED_ORIGINS = allowedOrigins;
    process.env.SPACES_ALLOWED_ORIGINS = allowedOrigins;
    console.log(`  Allowed origins: ${allowedOrigins}`);
  }

  // ─── NODE_PATH for the managed packages dir ──────────────
  const appNodeModules = path.join(projectDir, 'node_modules');
  const nodePaths = [MANAGED_NODE_MODULES, appNodeModules];
  for (const dir of nodePaths) {
    for (const target of [childEnv, process.env]) {
      const existing = target.NODE_PATH || '';
      if (!existing.includes(dir)) {
        target.NODE_PATH = existing
          ? `${dir}${path.delimiter}${existing}`
          : dir;
      }
    }
  }
  // Re-init module paths so require() in this process picks up the new NODE_PATH
  require('module').Module._initPaths();

  console.log('');

  // ─── Background update check (non-blocking, repeats every 4h) ──
  // Checks npm registry, GitHub releases, and addon git repos.
  // Writes results to ~/.spaces/update-check.json for the UI banner.
  const updateCheckPath = path.join(SPACES_DIR, 'update-check.json');
  const checkForUpdates = async (logIfAvailable = true) => {
    try {
      const pkg = require(path.join(projectDir, 'package.json'));
      const currentVersion = pkg.version;
      const npmName = pkg.name || '@jlongo78/agent-spaces';
      const githubRepo = 'jlongo78/agent-spaces';

      // Check npm registry
      let npmLatest = null;
      try {
        const res = await fetch(`https://registry.npmjs.org/${encodeURIComponent(npmName)}/latest`, {
          signal: AbortSignal.timeout(5000),
        });
        if (res.ok) {
          const data = await res.json();
          npmLatest = data.version;
        }
      } catch { /* */ }

      // Check GitHub for latest release or tag (may be ahead of npm)
      let githubLatest = null;
      let githubPrerelease = false;
      let githubUrl = null;
      try {
        // Check releases first (includes pre-releases)
        const res = await fetch(`https://api.github.com/repos/${githubRepo}/releases?per_page=5`, {
          signal: AbortSignal.timeout(5000),
          headers: { 'Accept': 'application/vnd.github.v3+json', 'User-Agent': 'agent-spaces' },
        });
        if (res.ok) {
          const releases = await res.json();
          if (releases.length > 0) {
            // Find latest stable and latest pre-release
            const stable = releases.find(r => !r.prerelease && !r.draft);
            const pre = releases.find(r => r.prerelease && !r.draft);
            if (stable) {
              githubLatest = stable.tag_name.replace(/^v/, '');
              githubUrl = stable.html_url;
            }
            if (pre) {
              const preVersion = pre.tag_name.replace(/^v/, '');
              // If pre-release is newer than stable, surface it
              if (!githubLatest || require('./lib/version').isNewer(preVersion, githubLatest)) {
                githubLatest = preVersion;
                githubPrerelease = true;
                githubUrl = pre.html_url;
              }
            }
          }
        }
      } catch { /* */ }

      const npmAvailable = require('./lib/version').isNewer(npmLatest, currentVersion);
      const githubAvailable = require('./lib/version').isNewer(githubLatest, currentVersion);

      const result = {
        current: currentVersion,
        npm: npmLatest ? { latest: npmLatest, available: !!npmAvailable } : null,
        github: githubLatest ? { latest: githubLatest, prerelease: githubPrerelease, url: githubUrl, available: !!githubAvailable } : null,
        // Backward compat: 'available' and 'latest' still work for the existing banner
        latest: npmLatest || githubLatest || currentVersion,
        available: !!(npmAvailable || githubAvailable),
        checkedAt: Date.now(),
        name: npmName,
      };

      fs.writeFileSync(updateCheckPath, JSON.stringify(result, null, 2));

      if (npmAvailable && logIfAvailable) {
        console.log(`  Update available: ${currentVersion} → ${npmLatest} (run: npm i -g ${npmName})`);
      }
      if (githubAvailable && (!npmAvailable || githubLatest > npmLatest) && logIfAvailable) {
        console.log(`  GitHub ${githubPrerelease ? 'pre-release' : 'release'}: ${githubLatest} (${githubUrl || `https://github.com/${githubRepo}/releases`})`);
      }
    } catch { /* network error — skip silently */ }
  };
  checkForUpdates(true);
  setInterval(() => checkForUpdates(false), 4 * 3600_000);

  // Check for ~/.claude/ directory
  const claudeDir = path.join(os.homedir(), '.claude');
  if (!fs.existsSync(claudeDir)) {
    console.log('  Warning: ~/.claude/ not found. Have you used Claude Code yet?');
    console.log('');
  }

  // ─── Detect build type ────────────────────────────────────
  const standaloneServer = path.join(projectDir, '.next', 'standalone', 'server.js');
  const fullBuildDir = path.join(projectDir, '.next', 'BUILD_ID');
  const isStandalone = fs.existsSync(standaloneServer);
  const isFullBuild = fs.existsSync(fullBuildDir);

  if (!isStandalone && !isFullBuild) {
    console.error('  Error: No build found.');
    console.error('  Run "npm run build" first, or install via "npm install -g @jlongo78/agent-spaces".');
    process.exit(1);
  }

  // ─── Spawn Next.js ────────────────────────────────────────
  let next;
  if (isStandalone) {
    const parentNodeModules = path.join(projectDir, 'node_modules');
    const existingNodePath = childEnv.NODE_PATH || '';
    childEnv.NODE_PATH = existingNodePath
      ? `${parentNodeModules}${path.delimiter}${existingNodePath}`
      : parentNodeModules;

    next = spawn(process.execPath, [standaloneServer], {
      cwd: path.dirname(standaloneServer),
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...childEnv,
        PORT: String(NEXT_INTERNAL_PORT),
        HOSTNAME: bindHost,
        NODE_ENV: 'production',
      },
    });
  } else {
    next = spawn('npx', ['next', 'start', '--port', String(NEXT_INTERNAL_PORT)], {
      cwd: projectDir,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...childEnv,
        PORT: String(NEXT_INTERNAL_PORT),
        HOSTNAME: bindHost,
        NODE_ENV: 'production',
      },
      shell: true,
    });
  }

  let nextReady = false;

  next.stdout.on('data', (data) => {
    const msg = data.toString();
    process.stdout.write(msg);
    if (!nextReady && (msg.includes('Ready') || msg.includes('started server') || msg.includes('Listening') || msg.includes('localhost'))) {
      nextReady = true;
      console.log(`\n  Ready at http://localhost:${PORT}`);
      if (tlsCreds) console.log(`  Also at  https://localhost:${PORT}  (self-signed)`);
      console.log('');

      const isService = process.env.SPACES_SERVICE === '1';
      if (!isService) {
        // Desktop mode only answers its owner, so the browser is opened on a
        // one-time sign-in link rather than the bare address.
        const url = isMultiuser
          ? `http://localhost:${PORT}${basePath || ''}`
          : signInUrl(PORT, basePath, internalToken);
        if (!(isMultiuser ? openInBrowser(url) : openSignInLink(url))) {
          console.log(isMultiuser
            ? `  Open ${url} in your browser`
            : '  Run "spaces open" to open Spaces in your browser, signed in.');
        }
      }
    }
  });

  next.stderr.on('data', (data) => {
    const msg = data.toString();
    if (!msg.includes('Warning')) {
      process.stderr.write(data);
    }
  });

  next.on('exit', (code) => {
    if (code !== 0 && code !== null) {
      console.error(`  [spaces] Next.js exited with code ${code}`);
    }
    cleanup();
  });

  // ─── Proxy request handler (shared by HTTP and HTTPS) ────
  function proxyHandler(req, res) {
    // Desktop mode: refuse rebound hostnames before anything else, and take
    // one-time sign-in links here, where the single-use bookkeeping lives.
    // Everything else is authenticated by src/middleware.ts.
    if (!isMultiuser) {
      if (!desktopAuth.isLocalHost(req.headers.host, allowedHostList)) {
        res.writeHead(421, { 'Content-Type': 'text/plain' });
        res.end('Unrecognized host');
        return;
      }
      const signInPath = `${basePath || ''}/auth/desktop`;
      const reqPath = (req.url || '').split('?')[0];
      if (reqPath === signInPath || reqPath === `${signInPath}/`) {
        handleDesktopSignIn(req, res);
        return;
      }
    }

    // WebSocket paths are handled by the 'upgrade' event, but a plain
    // HTTP request to /ws (e.g. health check) should not be proxied to
    // Next.js which would 308-redirect it due to trailingSlash.
    const urlPath = (req.url || '').split('?')[0];
    // The multi-machine network routes are experimental, like federation.
    const networkPath = `${basePath || ''}/api/network`;
    if (!EXPERIMENTAL && (urlPath === networkPath || urlPath.startsWith(`${networkPath}/`))) {
      res.writeHead(404, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Connecting machines is experimental; start Spaces with --experimental to use it.' }));
      return;
    }
    if (urlPath === '/ws' || urlPath.endsWith('/ws') || urlPath.endsWith('/ws/')) {
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end('WebSocket endpoint');
      return;
    }

    const proxyReq = http.request(
      {
        hostname: '127.0.0.1',
        port: NEXT_INTERNAL_PORT,
        path: req.url,
        method: req.method,
        headers: req.headers,
      },
      (proxyRes) => {
        {
          res.writeHead(proxyRes.statusCode, proxyRes.headers);
          proxyRes.pipe(res);
        }
      }
    );
    proxyReq.on('error', () => {
      if (!res.headersSent) {
        res.writeHead(502);
        res.end('Server not ready yet. Refresh in a moment.');
      }
    });
    req.pipe(proxyReq);
  }

  /** Swaps a one-time sign-in code for the browser session cookie. */
  function handleDesktopSignIn(req, res) {
    const code = new URL(req.url || '/', 'http://localhost').searchParams.get('code') || '';
    const headers = { 'Cache-Control': 'no-store', 'Referrer-Policy': 'no-referrer' };
    if (req.method !== 'GET' || !loginCodes.redeem(code)) {
      res.writeHead(401, { ...headers, 'Content-Type': 'text/plain; charset=utf-8' });
      res.end('This sign-in link has expired or was already used. Run "spaces open" for a new one.');
      return;
    }
    const session = desktopAuth.signDesktop(internalToken, 'session', desktopAuth.SESSION_TTL_S);
    desktopAuth.signedInPage(res, desktopAuth.desktopCookie(session, basePath), `${basePath || ''}/`);
  }

  // ─── Federation ────────────────────────────────────────────
  // Off unless asked for. Serves the runtime to machines that have been paired
  // by keypair, on one named interface and nothing else. Refused outright in
  // multiuser mode: `/proto` has no per-pane ownership there (see the SECURITY
  // note in terminal-server.js), and federation must not become a way around
  // that rather than through it.
  function startFederationIfRequested({ isMultiuser }) {
    const iface = cliFlags.federate || savedConfig.federate;
    if (!iface) return;

    // Experimental (see lib/experimental.js): without the opt-in, neither the
    // listener nor the outbound peer links start.
    const federationGate = experimentalGate.gateFederation({ iface, experimental: EXPERIMENTAL });
    if (!federationGate.start) {
      for (const line of federationGate.lines || []) console.error(line);
      return;
    }

    if (isMultiuser) {
      console.error('  Federation refused: not available in multiuser mode.');
      console.error('  Panes have no per-user ownership yet, so a federated peer could reach any of them.');
      return;
    }

    try {
      const { loadOrCreateIdentity } = require('./lib/network/identity');
      const { openNodeDb } = require('./lib/network/nodes');
      const { createFederationListener } = require('./lib/network/listener');
      const { createAuditLog } = require('./lib/network/audit');
      const { createPeerLinks } = require('./lib/network/links');
      const { checkFederatedRequest } = require('./lib/network/policy');
      const { attachControlChannel } = require('./lib/runtime/control-channel');
      const { attachPaneSocket } = require('./lib/runtime/pane-socket');
      const { getRuntime, setFederationContext } = require('./terminal-server');

      // Where a federated peer is allowed to start a pane. Same list the web UI
      // offers locally, so "somewhere I develop" means one thing on this
      // machine rather than two.
      // Read afresh each time rather than captured once, so editing the list
      // takes effect without a restart — and so a peer connected right now is
      // confined by the current list, not the one that was in force when it
      // connected.
      const devDirectories = () => {
        try {
          const cfg = JSON.parse(
            fs.readFileSync(path.join(SPACES_DIR, 'config.json'), 'utf-8')
          );
          return Array.isArray(cfg.devDirectories) ? cfg.devDirectories : [];
        } catch {
          // No config, no permitted directories. `checkFederatedRequest` fails
          // closed on an empty list, which is the right answer here.
          return [];
        }
      };

      const identity = loadOrCreateIdentity({ dir: SPACES_DIR });
      const peerDb = openNodeDb();
      // What the pane route needs to relay a remote pane.
      setFederationContext({ identity, db: peerDb });

      // Outbound links: this node dials peers so the client never has to hold
      // a credential. Installed on the runtime, which exposes them as
      // `net.snapshot`.
      const links = createPeerLinks({
        identity,
        db: peerDb,
        onEvent: (nodeId, event) => {
          // A peer's event, re-emitted locally tagged with its machine so a
          // client can fold it into that machine's slot rather than its own.
          try {
            getRuntime().hub.emit('net.peer.event', { nodeId, event });
          } catch {
            /* runtime not up yet */
          }
        },
        onStatus: (nodeId, status) => {
          try {
            getRuntime().hub.emit('net.node.updated', { nodeId, status });
          } catch {
            /* runtime not up yet */
          }
        },
      });
      try {
        getRuntime().attachPeerLinks(links);
      } catch {
        /* runtime comes up with the terminal server; net.snapshot reports it */
      }

      const listener = createFederationListener({
        iface,
        allowPublic: cliFlags.federationAllowPublic === true,
        port: cliFlags.federationPort || savedConfig.federationPort || 3479,
        identity,
        db: openNodeDb(),
        audit: createAuditLog(),
        onPeer: (ws, node, target) => {
          const who = node.name || node.id;
          if (target.kind === 'pane') {
            // A peer watching one of this machine's panes. The same
            // `attachPaneSocket` a local client gets, so replay, controller
            // arbitration and exit frames behave identically — one controller
            // per pane now holds across the fleet rather than per machine.
            const readOnly = node.read_only === 1;
            attachPaneSocket(ws, getRuntime(), {
              paneId: target.paneId,
              // A read-only peer may watch but never type, so it is never
              // given the controller slot.
              mode: readOnly ? 'observe' : 'control',
              authenticated: true,
            });
            return;
          }
          // The authenticated socket becomes an ordinary control channel — the
          // same protocol a local client speaks — with the federated policy
          // gating every request on it. One dialect, two privilege levels.
          console.log(`  [federation] ${who} connected`);
          attachControlChannel(ws, getRuntime(), {
            authenticated: true,
            policy: (method, params) =>
              checkFederatedRequest({
                method,
                params,
                devDirs: devDirectories(),
                readOnly: node.read_only === 1,
              }),
          });
          ws.on('close', () => console.log(`  [federation] ${who} disconnected`));
        },
      });

      listener
        .listen()
        .then((info) => {
          console.log(`  Federation: ${info.address}:${info.port} (${info.kind})`);
          if (info.kind === 'public') {
            console.log('  WARNING: that is a public address. Anyone who can reach it can attempt to pair.');
          }
          console.log(`  This machine's key: ${identity.publicKey.slice(0, 16)}…`);
        })
        .catch((e) => console.error(`  Federation failed to start: ${e.message}`));
    } catch (e) {
      // A refused interface is a configuration error, not a crash: the rest of
      // the server is perfectly usable without federation.
      console.error(`  Federation not started: ${e.message}`);
    }
  }

  // ─── HTTP + HTTPS on a single port ─────────────────────────
  // Peek at the first byte of each connection: 0x16 = TLS ClientHello,
  // anything else = plain HTTP.  This lets mobile browsers that force
  // HTTPS-first connect to the same port without a separate listener.
  const httpServer = http.createServer(proxyHandler);
  const tlsCreds = ensureTlsCert();
  const httpsServer = tlsCreds ? https.createServer(tlsCreds, proxyHandler) : null;

  const { createTerminalServer } = require('./terminal-server');
  createTerminalServer(httpServer);
  if (httpsServer) createTerminalServer(httpsServer);

  startFederationIfRequested({ isMultiuser });

  // Wire up 'address()' on the inner servers so terminal-server can
  // discover the port, then emit 'listening' so its event handler fires.
  const patchAddress = (srv) => { srv.address = () => ({ port: PORT }); };
  patchAddress(httpServer);
  if (httpsServer) patchAddress(httpsServer);

  const dualServer = net.createServer((socket) => {
    // If client connects but sends nothing for 5s, destroy.
    // Once routed, clear the timeout so long-lived connections (WebSocket) aren't killed.
    socket.setTimeout(5000, () => socket.destroy());
    socket.once('data', (buf) => {
      socket.setTimeout(0);
      socket.pause();
      const target = (buf[0] === 0x16 && httpsServer) ? httpsServer : httpServer;
      target.emit('connection', socket);
      socket.unshift(buf);
      socket.resume();
    });
  });

  dualServer.listen(PORT, bindHost, () => {
    // Now that the port is bound, tell the inner servers they're "listening"
    // so terminal-server's mDNS and message watcher start up.
    httpServer.emit('listening');
    if (httpsServer) httpsServer.emit('listening');

    if (httpsServer) {
      console.log(`  Starting server on port ${PORT}  (HTTP + HTTPS)`);
    } else {
      console.log(`  Starting server on http://localhost:${PORT}`);
    }
    console.log('');
  });

  function cleanup() {
    next.kill();
    dualServer.close();
    process.exit(0);
  }

  process.on('SIGINT', cleanup);
  process.on('SIGTERM', cleanup);
}

// ─── Stop running server ─────────────────────────────────────
function findPidsOnPort(port) {
  const pids = new Set();
  if (process.platform === 'win32') {
    try {
      const output = execFileSync('netstat', ['-ano'], { encoding: 'utf-8' });
      for (const line of output.split(String.fromCharCode(10))) {
        if (line.includes(':' + port + ' ') && line.includes('LISTENING')) {
          const parts = line.trim().split(/\s+/);
          const pid = parseInt(parts[parts.length - 1], 10);
          if (pid > 0) pids.add(pid);
        }
      }
    } catch {}
  } else {
    try {
      const output = execFileSync('lsof', ['-ti', `tcp:${port}`], { encoding: 'utf-8' }).trim();
      for (const p of output.split('\n')) { if (p.trim()) pids.add(parseInt(p.trim(), 10)); }
    } catch {}
    try {
      const output = execFileSync('fuser', [`${port}/tcp`], { encoding: 'utf-8', stdio: ['pipe', 'pipe', 'pipe'] }).trim();
      for (const p of output.split(/\s+/)) { if (p.trim()) pids.add(parseInt(p.trim(), 10)); }
    } catch {}
    // Neither tool exists on most slim images; the kernel has the answer too.
    if (!pids.size && process.platform === 'linux') {
      for (const pid of require('./lib/proc-ports').pidsListeningOn(Number(port))) pids.add(pid);
    }
  }
  return [...pids].filter(p => !isNaN(p) && p > 0);
}

function stopServer(overridePort) {
  let savedConfig = {};
  if (fs.existsSync(CONFIG_PATH)) {
    try { savedConfig = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8')); } catch {}
  }
  const port = overridePort || parseInt(process.env.SPACES_PORT || '', 10) || savedConfig.port || 3457;
  // `spaces stop` runs without startServer() ever setting NEXT_INTERNAL_PORT;
  // derive it the same way startServer() does.
  const ports = [port, port - 57];
  let killed = 0;

  for (const p of ports) {
    for (const pid of findPidsOnPort(p)) {
      try {
        process.kill(pid, 'SIGTERM');
        killed++;
      } catch {}
    }
  }

  if (killed > 0) {
    console.log(`  Stopped ${killed} process(es) on port ${port}`);
    // Wait for ports to be released, escalate to SIGKILL if needed
    const deadline = Date.now() + 2000;
    let escalated = false;
    while (Date.now() < deadline) {
      const remaining = ports.flatMap(p => findPidsOnPort(p));
      if (remaining.length === 0) break;
      if (!escalated && Date.now() > deadline - 1000) {
        for (const pid of remaining) {
          try { process.kill(pid, 'SIGKILL'); } catch {}
        }
        escalated = true;
      }
      if (process.platform === 'win32') { require('child_process').spawnSync('powershell', ['-Command', 'Start-Sleep -Milliseconds 200']); } else { require('child_process').spawnSync('sleep', ['0.2']); }
    }
  } else {
    console.log(`  No running server found on port ${port}`);
  }
}

