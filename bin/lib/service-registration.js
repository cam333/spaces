'use strict';

/**
 * Whether a Spaces OS service is registered on this machine — asked the same
 * way `spaces service status` asks each platform:
 *
 *   - Windows: the "Spaces" scheduled task exists (schtasks /Query), and its
 *     action is the Spaces wrapper — a task of the same name from anything
 *     else does not count.
 *   - macOS:   launchd has the com.agentspaces.spaces job (launchctl list).
 *   - Linux:   a spaces.service unit is installed, user or system level.
 *
 * The npm postinstall hook restarts the service only when this is true. It
 * used to run `spaces service stop` unconditionally, which on Windows kills
 * whatever node process listens on the Spaces ports — on every global
 * install, service or not.
 *
 * `run` and `exists` are injectable for tests. `run(cmd, args)` returns
 * stdout, or throws when the command fails.
 */

const path = require('path');
const os = require('os');
const fs = require('fs');

const TASK_NAME = 'Spaces';
const LABEL = 'com.agentspaces.spaces';
const SERVICE_NAME = 'spaces';

function defaultRun(cmd, args) {
  return require('child_process').execFileSync(cmd, args, {
    encoding: 'utf-8',
    stdio: ['ignore', 'pipe', 'ignore'],
    timeout: 10000,
    windowsHide: true,
  });
}

function serviceInstalled({
  platform = process.platform,
  homedir = os.homedir(),
  run = defaultRun,
  exists = fs.existsSync,
} = {}) {
  try {
    if (platform === 'win32') {
      const xml = String(run('schtasks', ['/Query', '/TN', TASK_NAME, '/XML']));
      return /spaces-service\.cmd/i.test(xml);
    }
    if (platform === 'darwin') {
      const out = String(run('launchctl', ['list']));
      return out.split('\n').some((line) => line.includes(LABEL));
    }
    if (platform === 'linux') {
      return (
        exists(path.join(homedir, '.config', 'systemd', 'user', `${SERVICE_NAME}.service`)) ||
        exists(`/etc/systemd/system/${SERVICE_NAME}.service`)
      );
    }
  } catch {
    // The query failed: no such task, no launchctl, ... — not installed.
  }
  return false;
}

module.exports = { serviceInstalled, TASK_NAME, LABEL, SERVICE_NAME };
