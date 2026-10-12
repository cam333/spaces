import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { findUnknownFlag } from '../../bin/lib/cli-flags.js';

describe('findUnknownFlag', () => {
  it('accepts every flag the launcher recognises', () => {
    const accepted: string[][] = [
      [],
      ['--setup'],
      ['--port', '4000'],
      ['--multiuser', '--experimental'],
      ['--desktop'],
      ['--no-multiuser'],
      ['--federate', 'tailscale0', '--federation-port', '3480', '--federation-public-interface'],
      ['--base-path', '/spaces'],
      ['--remote', 'me@host'],
      ['--help'], ['-h'],
      ['--version'], ['-v'],
      ['whoami', '--federate', '100.64.0.1'],
      ['tui', '--desktop', '--port', '4000', '--url', 'ws://127.0.0.1:4000', '--token', 't'],
      ['tui', '--remote', 'me@host'],
      ['--remote', 'me@host', '--yes'], ['--remote', 'me@host', '-y'],
      ['status'], ['status', '--porcelain'], ['restart'],
      ['start', '--print-token'], ['start', '--print-token', '--print-login'], ['open', '--remote', 'me@host'],
      ['start', '--multiuser', '--experimental'],
      ['stop', '--port', '4000'],
      ['open'], ['signout'], ['guide'],
    ];
    for (const args of accepted) expect(findUnknownFlag(args), args.join(' ')).toBeNull();
  });

  it('does not judge the arguments of commands handled by another script', () => {
    expect(findUnknownFlag(['service', 'install', '--whatever'])).toBeNull();
    expect(findUnknownFlag(['reset-totp', '-weird-user'])).toBeNull();
    expect(findUnknownFlag(['upgrade', '--force'])).toBeNull();
    expect(findUnknownFlag(['uninstall', '--yes'])).toBeNull();
  });

  it('reports a flag nothing recognises', () => {
    expect(findUnknownFlag(['--verison'])).toBe('--verison');
    expect(findUnknownFlag(['--port=4000'])).toBe('--port=4000');
    expect(findUnknownFlag(['tui', '--bogus'])).toBe('--bogus');
    expect(findUnknownFlag(['-x'])).toBe('-x');
  });

  it('does not mistake a flag value for a flag', () => {
    expect(findUnknownFlag(['--token', '-starts-with-dash'])).toBeNull();
    expect(findUnknownFlag(['--base-path', '-odd'])).toBeNull();
  });
});

// The launcher itself, run with a throwaway HOME so nothing touches the real
// ~/.spaces, and on a spare port. None of these cases binds a port.
describe('spaces CLI', () => {
  const launcher = path.join(__dirname, '..', '..', 'bin', 'spaces.js');
  const version = JSON.parse(
    fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf-8'),
  ).version;
  let home: string;

  beforeAll(() => {
    home = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-cli-test-'));
  });
  afterAll(() => {
    fs.rmSync(home, { recursive: true, force: true });
  });

  const run = (args: string[], env: Record<string, string> = {}) => {
    const childEnv: Record<string, string | undefined> = {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      ...env,
    };
    for (const k of ['SPACES_MULTIUSER', 'SPACES_EXPERIMENTAL', 'SPACES_PORT', '__SPACES_HEAP']) {
      if (!(k in env)) delete childEnv[k];
    }
    return spawnSync(process.execPath, [launcher, ...args], {
      env: childEnv as NodeJS.ProcessEnv,
      encoding: 'utf-8',
      timeout: 30000,
    });
  };

  it('--version and -v print the package version and exit 0', () => {
    for (const flag of ['--version', '-v']) {
      const r = run([flag]);
      expect(r.status).toBe(0);
      expect(r.stdout.trim()).toBe(version);
    }
  });

  it('an unknown flag exits 2 with a pointer to --help', () => {
    const r = run(['--no-such-flag', '--port', '4491']);
    expect(r.status).toBe(2);
    expect(r.stderr).toContain('Unknown option: --no-such-flag');
    expect(r.stderr).toContain('spaces --help');
  });

  it('--help lists --experimental and marks multiuser and federation experimental', () => {
    const r = run(['--help']);
    expect(r.status).toBe(0);
    expect(r.stdout).toContain('--experimental');
    expect(r.stdout).toMatch(/--multiuser\s+\[experimental\]/);
    expect(r.stdout).toMatch(/--federate <iface>\s+\[experimental\]/);
    expect(r.stdout).toContain('--version');
  });

  it('refuses multiuser mode without --experimental, exit 1', () => {
    for (const [args, env] of [
      [['--multiuser', '--port', '4492'], {}],
      [['--port', '4492'], { SPACES_MULTIUSER: '1' }],
    ] as const) {
      const r = run([...args], { ...env });
      expect(r.status).toBe(1);
      expect(r.stderr).toMatch(/multiuser mode is experimental/i);
      expect(r.stderr).toContain('--experimental');
    }
  });

  it('refuses a saved multiuser setting too, unless SPACES_EXPERIMENTAL is set', () => {
    fs.mkdirSync(path.join(home, '.spaces'), { recursive: true });
    fs.writeFileSync(path.join(home, '.spaces', 'server.json'), JSON.stringify({ multiuser: true }));
    try {
      const r = run(['--port', '4493']);
      expect(r.status).toBe(1);
      expect(r.stderr).toContain('server.json');
    } finally {
      fs.rmSync(path.join(home, '.spaces', 'server.json'), { force: true });
    }
  });
});
