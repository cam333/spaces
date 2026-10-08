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
