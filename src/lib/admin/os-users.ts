/**
 * Enumerate real OS login accounts on the host so the admin UI can pin a
 * Spaces user's shell mapping to an actual machine user (dropdown) instead of
 * accepting freeform text. Also used to validate the shell user server-side —
 * cross-platform, unlike the old `id`-only check which rejected every user on
 * Windows.
 */
import { execFileSync } from 'child_process';

// Well-known system / built-in / privileged accounts to hide from the picker.
// (Privileged ones are also rejected by validateShellUserName — review L2.)
const SYSTEM_ACCOUNTS = new Set([
  'root', 'daemon', 'bin', 'sys', 'sync', 'games', 'man', 'lp', 'mail', 'news',
  'uucp', 'proxy', 'backup', 'list', 'irc', 'gnats', 'nobody', 'systemd',
  'administrator', 'admin', 'guest', 'defaultaccount', 'wdagutilityaccount',
]);

function isSystemName(name: string): boolean {
  const n = name.toLowerCase();
  if (SYSTEM_ACCOUNTS.has(n)) return true;
  if (n.startsWith('_')) return true;          // macOS service accounts
  if (n.startsWith('systemd-')) return true;   // Linux systemd accounts
  if (n.startsWith('nt ')) return true;        // Windows "NT AUTHORITY\..." etc.
  return false;
}

function uniqueSorted(names: string[]): string[] {
  return Array.from(new Set(names)).sort((a, b) => a.localeCompare(b));
}

/** Parse Linux /etc/passwd (or `getent passwd`) into real login usernames. */
export function parsePasswdUsers(content: string): string[] {
  const out: string[] = [];
  for (const line of content.split('\n')) {
    const parts = line.split(':');
    if (parts.length < 7) continue;
    const name = parts[0].trim();
    const uid = parseInt(parts[2], 10);
    const shell = parts[6].trim();
    if (!name || isSystemName(name)) continue;
    if (!Number.isInteger(uid) || uid < 1000 || uid >= 65534) continue;
    if (!shell || /(?:nologin|false)$/.test(shell)) continue;
    out.push(name);
  }
  return uniqueSorted(out);
}

/** Parse a newline-separated list of usernames (Windows Get-LocalUser / macOS dscl). */
export function parseSimpleUserList(output: string): string[] {
  const out: string[] = [];
  for (const line of output.split(/\r?\n/)) {
    const name = line.trim();
    if (!name || isSystemName(name)) continue;
    out.push(name);
  }
  return uniqueSorted(out);
}

/** List real OS login users on this host. Returns [] on any failure. */
export function listOsUsers(): string[] {
  try {
    if (process.platform === 'win32') {
      // Enabled local accounts only.
      const out = execFileSync('powershell', ['-NoProfile', '-Command',
        'Get-LocalUser | Where-Object { $_.Enabled } | Select-Object -ExpandProperty Name',
      ], { encoding: 'utf-8', timeout: 10000 });
      return parseSimpleUserList(out);
    }
    if (process.platform === 'darwin') {
      const out = execFileSync('dscl', ['.', '-list', '/Users'], { encoding: 'utf-8', timeout: 10000 });
      return parseSimpleUserList(out);
    }
    // Linux and other POSIX
    const out = execFileSync('getent', ['passwd'], { encoding: 'utf-8', timeout: 10000 });
    return parsePasswdUsers(out);
  } catch {
    // Fallback: read /etc/passwd directly on POSIX if getent is unavailable.
    if (process.platform !== 'win32') {
      try {
        const fs = require('fs') as typeof import('fs');
        return parsePasswdUsers(fs.readFileSync('/etc/passwd', 'utf-8'));
      } catch { /* ignore */ }
    }
    return [];
  }
}

/** Whether a name is a real OS login user on this host. */
export function osUserExists(name: string): boolean {
  return listOsUsers().includes(name);
}
