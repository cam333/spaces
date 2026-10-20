'use strict';

// After `npm install -g`, is `spaces` runnable? npm puts its commands in
// one folder (the prefix itself on Windows, `<prefix>/bin` elsewhere), and
// that folder is not always on PATH: Node installed some other way than its
// installer, a custom prefix, or a terminal opened before the installer
// changed PATH. Then the install succeeds and `spaces` is "not recognized".
//
// The postinstall hook uses this to check, to put the folder on the user's
// PATH on Windows (what Node's own installer does), and to say how to run
// Spaces right away. A process cannot change its parent shell's PATH, so the
// terminal that ran npm needs the one line printed here, or a new terminal.

const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

/** Where npm put the `spaces` command for a global install under `prefix`. */
function globalBinDir(prefix, platform = process.platform) {
  if (!prefix) return null;
  return platform === 'win32' ? prefix : path.posix.join(prefix, 'bin');
}

function normalize(dir, platform) {
  let d = String(dir).trim().replace(/^"(.*)"$/, '$1');
  if (platform === 'win32') {
    d = d.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
  } else {
    d = d.replace(/\/+$/, '');
  }
  return d;
}

/** Whether `dir` is one of the entries of the PATH string `pathValue`. */
function onPath(dir, pathValue, platform = process.platform) {
  if (!dir || !pathValue) return false;
  const want = normalize(dir, platform);
  const sep = platform === 'win32' ? ';' : ':';
  return String(pathValue).split(sep).some((p) => p && normalize(p, platform) === want);
}

/** The PATH of the user's own environment on Windows (the registry), or null. */
function readUserPath(run = execFileSync, name = 'Path') {
  try {
    const out = run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      "[Environment]::GetEnvironmentVariable($env:SPACES_VAR_NAME,'User')"],
    { encoding: 'utf-8', timeout: 15000, windowsHide: true, env: { ...process.env, SPACES_VAR_NAME: name } });
    return String(out).trim();
  } catch {
    return null;
  }
}

/**
 * Appends `dir` to the user's PATH on Windows. Through .NET rather than
 * `setx`, which truncates at 1024 characters, and which, unlike this, does
 * not tell open programs (Explorer, new terminals) that PATH changed. The
 * folder travels in an environment variable, never in the command text.
 */
function addToUserPath(dir, run = execFileSync, name = 'Path') {
  try {
    run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      "$d = $env:SPACES_BIN_DIR; $n = $env:SPACES_VAR_NAME; $p = [Environment]::GetEnvironmentVariable($n,'User'); " +
      "if (-not $p) { $p = $d } elseif (($p -split ';') -notcontains $d) { $p = $p.TrimEnd(';') + ';' + $d }; " +
      "[Environment]::SetEnvironmentVariable($n, $p, 'User')"],
    { encoding: 'utf-8', timeout: 15000, windowsHide: true, env: { ...process.env, SPACES_BIN_DIR: dir, SPACES_VAR_NAME: name } });
    return true;
  } catch {
    return false;
  }
}

/** The lines that make `spaces` runnable in the terminal that ran npm. */
function thisTerminalLines(dir, platform = process.platform) {
  if (platform === 'win32') {
    return [
      `PowerShell:  $env:Path = "${dir};" + $env:Path`,
      `cmd:         set "PATH=${dir};%PATH%"`,
    ];
  }
  return [`export PATH="${dir}:$PATH"`];
}

/**
 * What to tell the user, and whether to fix the user PATH, given where npm
 * put the command and the two PATHs that matter: this terminal's, and (on
 * Windows) the one new terminals will get. Pure, for tests.
 *
 * @returns {null | { addToUserPath: boolean, lines: string[] }}
 */
function plan({ dir, terminalPath, userPath, platform = process.platform, allowFix = true }) {
  if (!dir || onPath(dir, terminalPath, platform)) return null;
  const lines = [];
  let fix = false;
  if (platform === 'win32') {
    const inUser = userPath !== null && onPath(dir, userPath, platform);
    if (!inUser && allowFix && userPath !== null) {
      fix = true;
      lines.push(`Added ${dir} to your PATH, so "spaces" works in every new terminal.`);
    } else if (!inUser) {
      lines.push(`"spaces" is in ${dir}, which is not on your PATH. Add it in`);
      lines.push('Settings > System > About > Advanced system settings > Environment Variables.');
    } else {
      lines.push('"spaces" works in any new terminal.');
    }
    lines.push('To use it in this terminal now, run:');
  } else {
    lines.push(`"spaces" is in ${dir}, which is not on your PATH.`);
    lines.push('Add this line to your shell profile (~/.bashrc, ~/.zshrc), and run it now:');
  }
  return { addToUserPath: fix, lines: [...lines, ...thisTerminalLines(dir, platform).map((l) => `  ${l}`)] };
}

/**
 * Writes to the terminal itself. npm hides what install scripts print unless
 * they fail, and a message about how to run the thing just installed is
 * worth getting past that. Falls back to stderr where there is no terminal.
 */
function sayToTerminal(text, platform = process.platform) {
  const device = platform === 'win32' ? '\\\\.\\CONOUT$' : '/dev/tty';
  try {
    const fd = fs.openSync(device, 'w');
    try { fs.writeSync(fd, text); } finally { fs.closeSync(fd); }
    return true;
  } catch {
    try { process.stderr.write(text); } catch { /* nowhere to say it */ }
    return false;
  }
}

module.exports = { globalBinDir, onPath, readUserPath, addToUserPath, thisTerminalLines, plan, sayToTerminal };
