// tests/lib/path-check.test.ts
import { describe, it, expect } from 'vitest';
import { globalBinDir, onPath, plan, thisTerminalLines } from '../../bin/lib/path-check.js';

// After `npm install -g`, `spaces` must run. When npm's command folder is not
// on PATH the install says so, fixes the user PATH on Windows, and prints the
// line that makes it work in the terminal that ran npm.

const WIN_DIR = 'C:\\Users\\pat\\AppData\\Roaming\\npm';

describe('globalBinDir', () => {
  it('is the prefix on Windows and prefix/bin elsewhere', () => {
    expect(globalBinDir(WIN_DIR, 'win32')).toBe(WIN_DIR);
    expect(globalBinDir('/home/pat/.local', 'linux')).toBe('/home/pat/.local/bin');
    expect(globalBinDir('', 'linux')).toBeNull();
  });
});

describe('onPath', () => {
  it('matches Windows entries without regard to case, slashes, quotes or a trailing slash', () => {
    const p = `C:\\Windows;"c:/users/PAT/AppData/Roaming/npm/";C:\\Program Files\\nodejs`;
    expect(onPath(WIN_DIR, p, 'win32')).toBe(true);
    expect(onPath(WIN_DIR, 'C:\\Windows;C:\\Program Files\\nodejs', 'win32')).toBe(false);
  });

  it('matches POSIX entries exactly, trailing slash aside', () => {
    expect(onPath('/home/pat/.local/bin', '/usr/bin:/home/pat/.local/bin/', 'linux')).toBe(true);
    expect(onPath('/home/pat/.local/bin', '/usr/bin:/home/PAT/.local/bin', 'linux')).toBe(false);
  });
});

describe('plan', () => {
  it('says nothing when the command already runs here', () => {
    expect(plan({ dir: WIN_DIR, terminalPath: `C:\\Windows;${WIN_DIR}`, userPath: '', platform: 'win32' })).toBeNull();
  });

  it('adds the folder to the user PATH on Windows when it is in neither PATH', () => {
    const p = plan({ dir: WIN_DIR, terminalPath: 'C:\\Windows', userPath: 'C:\\Tools', platform: 'win32' })!;
    expect(p.addToUserPath).toBe(true);
    expect(p.lines.join('\n')).toContain(`Added ${WIN_DIR} to your PATH`);
    expect(p.lines.join('\n')).toContain(`$env:Path = "${WIN_DIR};" + $env:Path`);
    expect(p.lines.join('\n')).toContain(`set "PATH=${WIN_DIR};%PATH%"`);
  });

  it('only explains this terminal when new terminals already have it', () => {
    const p = plan({ dir: WIN_DIR, terminalPath: 'C:\\Windows', userPath: WIN_DIR, platform: 'win32' })!;
    expect(p.addToUserPath).toBe(false);
    expect(p.lines.join('\n')).toContain('works in any new terminal');
  });

  it('changes nothing when told not to, or when the user PATH cannot be read', () => {
    for (const args of [{ allowFix: false, userPath: '' }, { allowFix: true, userPath: null }]) {
      const p = plan({ dir: WIN_DIR, terminalPath: 'C:\\Windows', platform: 'win32', ...args })!;
      expect(p.addToUserPath).toBe(false);
      expect(p.lines.join('\n')).toContain('Environment Variables');
    }
  });

  it('never edits shell profiles elsewhere; it prints the line', () => {
    const p = plan({ dir: '/opt/npm/bin', terminalPath: '/usr/bin', userPath: null, platform: 'linux' })!;
    expect(p.addToUserPath).toBe(false);
    expect(p.lines.join('\n')).toContain('export PATH="/opt/npm/bin:$PATH"');
  });
});

describe('thisTerminalLines', () => {
  it('gives PowerShell and cmd lines on Windows', () => {
    expect(thisTerminalLines(WIN_DIR, 'win32')).toHaveLength(2);
  });
});
