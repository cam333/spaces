#!/usr/bin/env node

'use strict';

/**
 * npm postinstall hook — runs after `npm i -g @jlongo78/agent-spaces`.
 * Clears stale version cache and restarts the service if one is installed.
 */

const { execFileSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

const SPACES_DIR = path.join(os.homedir(), '.spaces');

// Skip during local dev `npm install` (no global flag, not inside global node_modules)
if (!process.env.npm_config_global) {
  const isGlobalPath = __dirname.includes(path.join('node_modules', '@jlongo78', 'agent-spaces'));
  if (!isGlobalPath) process.exit(0);
}

function log(msg) { console.log(`  ${msg}`); }

// Clear stale update-check cache
try { fs.unlinkSync(path.join(SPACES_DIR, 'update-check.json')); } catch {}

// Write fresh cache with the just-installed version
try {
  const pkg = require(path.join(__dirname, '..', 'package.json'));
  fs.mkdirSync(SPACES_DIR, { recursive: true });
  fs.writeFileSync(path.join(SPACES_DIR, 'update-check.json'), JSON.stringify({
    current: pkg.version,
    latest: pkg.version,
    available: false,
    checkedAt: Date.now(),
    name: pkg.name,
  }, null, 2));
  log(`Installed v${pkg.version}`);
} catch {}

// Make sure "spaces" runs: npm's command folder is not always on PATH.
// SPACES_NO_PATH_FIX=1 reports without changing the user's PATH.
try {
  const pc = require('./lib/path-check');
  // The package is <global node_modules>/@jlongo78/agent-spaces, and the
  // global node_modules is <prefix>/node_modules on Windows and
  // <prefix>/lib/node_modules elsewhere.
  const globalModules = path.resolve(__dirname, '..', '..', '..');
  const derived = path.basename(globalModules) === 'node_modules'
    ? (process.platform === 'win32' ? path.dirname(globalModules) : path.dirname(path.dirname(globalModules)))
    : null;
  const dir = pc.globalBinDir(derived || process.env.npm_config_prefix);
  const isWin = process.platform === 'win32';
  const terminalPath = process.env.PATH || process.env.Path || '';
  const needs = dir && !pc.onPath(dir, terminalPath);
  const todo = needs && pc.plan({
    dir,
    terminalPath,
    userPath: isWin ? pc.readUserPath() : null,
    allowFix: !process.env.SPACES_NO_PATH_FIX,
  });
  if (todo) {
    const added = todo.addToUserPath ? pc.addToUserPath(dir) : false;
    const lines = todo.addToUserPath && !added
      ? todo.lines.map((l) => l.startsWith('Added ') ? `"spaces" is in ${dir}, which is not on your PATH.` : l)
      : todo.lines;
    pc.sayToTerminal(`\n  Spaces is installed.\n${lines.map((l) => `  ${l}`).join('\n')}\n\n`);
  }
} catch {
  // Advice only: an install never fails over it.
}

// Restart the service, but only when one is registered. `spaces service stop`
// kills node processes on the Spaces ports, so running it on an install with
// no service would take down a server the user started by hand.
const { serviceInstalled } = require('./lib/service-registration');
if (!serviceInstalled()) process.exit(0);

const serviceScript = path.join(__dirname, 'spaces-service.js');
try {
  execFileSync(process.execPath, [serviceScript, 'stop'], { stdio: 'pipe', timeout: 10000 });
  execFileSync(process.execPath, [serviceScript, 'start'], { stdio: 'inherit', timeout: 10000 });
  log('Service restarted with new version');
} catch {
  // Service wasn't running or couldn't restart — that's fine
}
