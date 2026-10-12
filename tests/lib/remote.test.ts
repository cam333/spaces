import { describe, it, expect } from 'vitest';
import {
  validateDestination,
  sshStartArgs,
  sshTunnelArgs,
  parseStartOutput,
  parseStartToken,
  classifyStartFailure,
  installHint,
  splitRemote,
} from '../../bin/lib/remote.js';

describe('validateDestination', () => {
  it('accepts the forms ssh takes as a destination', () => {
    for (const d of ['devbox', 'you@devbox', 'you@devbox.example.com', 'box-1',
      'ssh://you@devbox:2222', 'ssh://[::1]:22', 'you@100.64.0.7']) {
      expect(validateDestination(d)).toBe(d);
    }
  });

  it('refuses anything ssh would read as an option', () => {
    expect(() => validateDestination('-oProxyCommand=calc')).toThrow(/not a destination/);
    expect(() => validateDestination('-p')).toThrow();
  });

  it('refuses shell metacharacters, spaces and empty input', () => {
    for (const d of ['a;b', 'host name', 'host$(x)', 'a`b`', '', 'a@b@c']) {
      expect(() => validateDestination(d)).toThrow();
    }
  });
});

describe('ssh arguments', () => {
  it('ends option parsing before the destination', () => {
    const start = sshStartArgs('you@devbox');
    expect(start.slice(-3)).toEqual(['--', 'you@devbox', 'spaces start --print-token']);
    const tunnel = sshTunnelArgs('you@devbox', 40001, 3457);
    expect(tunnel.slice(-2)).toEqual(['--', 'you@devbox']);
  });

  it('forwards loopback to loopback and fails fast if it cannot', () => {
    const tunnel = sshTunnelArgs('h', 40001, 3457);
    expect(tunnel).toContain('127.0.0.1:40001:127.0.0.1:3457');
    expect(tunnel).toContain('ExitOnForwardFailure=yes');
    expect(tunnel).toContain('-N');
  });

  it('retries through a login shell that also loads nvm, with a constant command', () => {
    const retry = sshStartArgs('h', { loginShell: true }).at(-1);
    expect(retry).toBe(
      `"\${SHELL:-sh}" -lc '[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1; PATH="$HOME/.local/bin:$PATH"; spaces start --print-token'`,
    );
  });
});

describe('parseStartOutput', () => {
  it('finds the port line among the rest of the output', () => {
    expect(parseStartOutput('  Spaces is already running\nSPACES_PORT=3457\n')).toBe(3457);
    expect(parseStartOutput('SPACES_PORT=4000\r\n')).toBe(4000);
  });

  it('is null without one, or with a nonsense port', () => {
    expect(parseStartOutput('bash: spaces: command not found')).toBeNull();
    expect(parseStartOutput('SPACES_PORT=0')).toBeNull();
    expect(parseStartOutput('SPACES_PORT=99999')).toBeNull();
  });
});

describe('parseStartToken', () => {
  it('finds a signed token on its own line', () => {
    expect(parseStartToken('  running\nSPACES_PORT=3457\nSPACES_TOKEN=eyJr.AbC-_9\n')).toBe('eyJr.AbC-_9');
  });

  it('refuses anything that is not the token shape', () => {
    expect(parseStartToken('SPACES_TOKEN=a b')).toBeNull();
    expect(parseStartToken('SPACES_TOKEN=$(rm -rf ~).x')).toBeNull();
    expect(parseStartToken('SPACES_TOKEN=nodot')).toBeNull();
    expect(parseStartToken('nothing here')).toBeNull();
  });
});

describe('classifyStartFailure', () => {
  it('tells an ssh failure from a missing install from anything else', () => {
    expect(classifyStartFailure(255, 'Permission denied (publickey).')).toBe('ssh');
    expect(classifyStartFailure(127, 'bash: line 1: spaces: command not found')).toBe('missing');
    expect(classifyStartFailure(1, 'zsh:1: command not found: spaces')).toBe('failed');
    expect(classifyStartFailure(1, "'spaces' is not recognized as an internal or external command")).toBe('missing');
    expect(classifyStartFailure(1, 'The Spaces server exited before it came up')).toBe('failed');
  });
});

describe('installHint', () => {
  it('prints the command to run, not a guess at what happened', () => {
    expect(installHint('you@devbox')).toContain("ssh you@devbox 'npm install -g @jlongo78/agent-spaces'");
  });
});

describe('splitRemote', () => {
  it('lifts --remote out of argv', () => {
    expect(splitRemote(['--remote', 'you@devbox', '--token', 't']))
      .toEqual({ remote: 'you@devbox', rest: ['--token', 't'] });
  });

  it('leaves a valueless trailing --remote to be reported', () => {
    expect(splitRemote(['--remote'])).toEqual({ remote: null, rest: ['--remote'] });
  });
});
