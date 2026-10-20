#!/usr/bin/env node

'use strict';

const { execFileSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

const CORE_PACKAGE = '@jlongo78/agent-spaces';

// Resolve the target user's home directory.
// On Windows, if running from a separate admin account, find the user
// who actually has Claude Code data (.claude/ directory).
function resolveTargetHome() {
  const currentHome = os.homedir();
  if (process.platform !== 'win32') return currentHome;
  if (fs.existsSync(path.join(currentHome, '.claude'))) return currentHome;
  const usersDir = path.dirname(currentHome);
  const skip = new Set(['Public', 'Default', 'Default User', 'All Users']);
  try {
    const claudeUsers = fs.readdirSync(usersDir)
      .filter(name => !skip.has(name) && !name.startsWith('.'))
      .filter(name => fs.existsSync(path.join(usersDir, name, '.claude')));
    if (claudeUsers.length === 1) return path.join(usersDir, claudeUsers[0]);
  } catch {}
  return currentHome;
}

const TARGET_HOME = resolveTargetHome();
const SPACES_DIR = path.join(TARGET_HOME, '.spaces');

// ─── Helpers ──────────────────────────────────────────────────
function log(msg) { console.log(`  ${msg}`); }
function logOk(msg) { console.log(`  ✓ ${msg}`); }
function logErr(msg) { console.error(`  ✗ ${msg}`); }
function logWarn(msg) { console.log(`  ! ${msg}`); }

/**
 * `--prefix` for npm when this copy was installed somewhere other than
 * npm's global folder — ~/.local, where `spaces --remote` and the install
 * script put it when that folder needs root — so an upgrade replaces this
 * copy instead of failing to write the root-owned one.
 */
function samePlaceArgs() {
  const { installPrefix } = require('./lib/path-check');
  const here = installPrefix(path.join(__dirname, '..'));
  if (!here) return [];
  let globalPrefix = '';
  try { globalPrefix = String(run('npm', ['prefix', '-g'], { quiet: true })).trim(); } catch { /* use ours */ }
  const same = (a, b) => process.platform === 'win32'
    ? path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase()
    : path.resolve(a) === path.resolve(b);
  return globalPrefix && same(here, globalPrefix) ? [] : ['--prefix', here];
}

function run(cmd, args, opts = {}) {
  const isWin = process.platform === 'win32';
  const safeArgs = isWin
    ? args.map(a => (a.includes(' ') ? '"' + a + '"' : a))
    : args;
  return execFileSync(cmd, safeArgs, {
    encoding: 'utf-8',
    stdio: opts.quiet ? ['pipe', 'pipe', 'pipe'] : 'inherit',
    shell: isWin,
    ...opts,
  });
}

function restartServiceIfRunning() {
  // Only through a registered service: `spaces service stop` kills the
  // processes on the Spaces ports, so with no service it would take down a
  // server started by hand and start nothing in its place.
  const { serviceInstalled } = require('./lib/service-registration');
  if (!serviceInstalled()) {
    log('If Spaces is running, "spaces restart" puts the new version in charge.');
    return;
  }
  const serviceScript = path.join(__dirname, 'spaces-service.js');
  try {
    log('Restarting service...');
    execFileSync(process.execPath, [serviceScript, 'stop'], { stdio: 'pipe', timeout: 10000 });
  } catch { /* may not be running */ }
  try {
    execFileSync(process.execPath, [serviceScript, 'start'], { stdio: 'inherit', timeout: 10000 });
    logOk('Service restarted with new version');
  } catch {
    log('Run "spaces service start" to start the new version');
  }
}

// ─── Upgrade ──────────────────────────────────────────────────
// Upgrade the core CLI via npm, in place.
function upgradeCorePackage() {
  log(`─── ${CORE_PACKAGE} ───`);

  let currentVersion = '0.0.0';
  try { currentVersion = require('../package.json').version; } catch {}

  let latestVersion;
  try {
    latestVersion = run('npm', ['view', CORE_PACKAGE, 'version'], { quiet: true }).trim();
  } catch {
    logErr(`Could not check npm for the latest ${CORE_PACKAGE} version`);
    return;
  }

  if (latestVersion === currentVersion) {
    logOk(`${CORE_PACKAGE} is already at the latest version (${currentVersion})`);
    return;
  }

  log(`Upgrading ${CORE_PACKAGE}: ${currentVersion} → ${latestVersion}`);
  try {
    run('npm', ['install', '-g', ...samePlaceArgs(), `${CORE_PACKAGE}@latest`]);
    logOk(`${CORE_PACKAGE} upgraded to ${latestVersion}`);
  } catch (err) {
    logErr(`Failed to upgrade ${CORE_PACKAGE}: ${err.message}`);
    log('You may need to run with elevated permissions (sudo / admin shell)');
    return;
  }

  // Clear stale update-check cache so the UI shows the correct version.
  try { fs.unlinkSync(path.join(SPACES_DIR, 'update-check.json')); } catch {}

  restartServiceIfRunning();
}

// ─── Uninstall ────────────────────────────────────────────────
function uninstall() {
  console.log(`\n  Uninstalling ${CORE_PACKAGE}...\n`);
  try {
    run('npm', ['uninstall', '-g', CORE_PACKAGE], { quiet: true });
    logOk('Removed the global spaces CLI');
  } catch {
    logWarn('Global spaces CLI not found or already removed');
  }
  logOk('Spaces uninstalled');
  log('User data remains at ~/.spaces/ — remove manually if desired');
  console.log('');
}

// ─── CLI ──────────────────────────────────────────────────────
const command = process.argv[2];

switch (command) {
  case 'uninstall':
    uninstall();
    break;

  case 'upgrade':
    console.log('\n  Upgrading Agent Spaces...\n');
    upgradeCorePackage();
    console.log('');
    break;

  default:
    logErr(`Unknown command: ${command}`);
    log('Usage: spaces <upgrade|uninstall>');
    process.exit(1);
}
