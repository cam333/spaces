import { describe, it, expect } from 'vitest';
import {
  inLoginShell,
  sshProbeArgs,
  parseProbe,
  nodeIsSupported,
  planRemote,
  installSpec,
  sshInstallArgs,
  classifyInstallFailure,
  installFailureHint,
  PACKAGE,
} from '../../bin/lib/remote.js';

describe('inLoginShell', () => {
  it('runs the command after loading nvm, inside single quotes', () => {
    expect(inLoginShell('spaces status')).toBe(
      `"\${SHELL:-sh}" -lc '[ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh" >/dev/null 2>&1; spaces status'`,
    );
  });

  it('refuses a command that could close the quotes', () => {
    expect(() => inLoginShell("echo 'x'")).toThrow(/single quote/);
  });
});

describe('the probe', () => {
  it('is one constant command after the destination', () => {
    const args = sshProbeArgs('you@devbox');
    expect(args.slice(-3, -1)).toEqual(['--', 'you@devbox']);
    expect(sshProbeArgs('other').at(-1)).toBe(args.at(-1));
  });

  it('asks only versions that have it for their status', () => {
    const probe = sshProbeArgs('h').at(-1);
    expect(probe).toContain('case "$v" in 0.*|"") ;; *) spaces status --porcelain');
  });

  it('reads a current install, running with agents', () => {
    expect(parseProbe('SPACES_NODE=v20.19.6\nSPACES_VERSION=1.0.0\nSPACES_RUNNING=1\nSPACES_PORT=3457\nSPACES_LIVE_AGENTS=3\n'))
      .toEqual({ node: '20.19.6', missing: false, version: '1.0.0', running: true, liveAgents: 3 });
  });

  it('reads an old install that cannot report its agents', () => {
    expect(parseProbe('SPACES_NODE=v20.19.6\nSPACES_VERSION=0.9.10\n'))
      .toEqual({ node: '20.19.6', missing: false, version: '0.9.10', running: null, liveAgents: null });
  });

  it('reads a machine without Spaces, or without Node.js too', () => {
    expect(parseProbe('SPACES_NODE=v22.12.0\nSPACES_MISSING=1\n'))
      .toMatchObject({ node: '22.12.0', missing: true, version: null });
    expect(parseProbe('SPACES_MISSING=1\n')).toMatchObject({ node: null, missing: true });
  });

  it('finds the version in a noisy --version line, and survives CRLF', () => {
    expect(parseProbe('SPACES_VERSION=Spaces 1.2.3-beta.1\r\n')?.version).toBe('1.2.3-beta.1');
    expect(parseProbe('SPACES_VERSION=\n')?.version).toBeNull();
  });

  it('is null for output it does not recognise, so --remote connects as before', () => {
    expect(parseProbe('')).toBeNull();
    expect(parseProbe("'\"${SHELL:-sh}\"' is not recognized as an internal or external command")).toBeNull();
  });
});

describe('nodeIsSupported', () => {
  it('wants 20.9 or newer', () => {
    expect(nodeIsSupported('20.9.0')).toBe(true);
    expect(nodeIsSupported('20.19.6')).toBe(true);
    expect(nodeIsSupported('22.0.0')).toBe(true);
    expect(nodeIsSupported('20.8.1')).toBe(false);
    expect(nodeIsSupported('18.20.0')).toBe(false);
    expect(nodeIsSupported(null)).toBe(false);
  });
});

describe('planRemote', () => {
  const at = (extra: object) => ({ node: '20.19.6', missing: false, version: '1.0.0', running: true, liveAgents: 0, ...extra });

  it('connects when the versions match, or when the probe found nothing', () => {
    expect(planRemote(at({}), '1.0.0')).toEqual({ action: 'connect' });
    expect(planRemote(null, '1.0.0')).toEqual({ action: 'connect' });
  });

  it('installs where Spaces is missing and Node.js is fit', () => {
    expect(planRemote(at({ missing: true, version: null }), '1.0.0')).toEqual({ action: 'install' });
  });

  it('cannot install without Node.js, or with one too old', () => {
    expect(planRemote(at({ missing: true, version: null, node: null }), '1.0.0')).toEqual({ action: 'no-node' });
    expect(planRemote(at({ missing: true, version: null, node: '18.20.0' }), '1.0.0'))
      .toEqual({ action: 'old-node', node: '18.20.0' });
  });

  it('updates an older install, carrying what it knows about running agents', () => {
    expect(planRemote(at({ version: '0.9.10', running: null, liveAgents: null }), '1.0.0'))
      .toEqual({ action: 'update', from: '0.9.10', running: null, liveAgents: null });
    expect(planRemote(at({ version: '1.0.0' }), '1.0.1'))
      .toEqual({ action: 'update', from: '1.0.0', running: true, liveAgents: 0 });
  });

  it('compares versions as numbers, not strings', () => {
    expect(planRemote(at({ version: '0.10.0' }), '0.9.10').action).toBe('newer');
    expect(planRemote(at({ version: '0.9.10' }), '0.10.0').action).toBe('update');
  });

  it('leaves a newer remote alone', () => {
    expect(planRemote(at({ version: '1.1.0' }), '1.0.0')).toEqual({ action: 'newer', remote: '1.1.0' });
  });
});

describe('installSpec', () => {
  it('installs exactly this machine\'s version', () => {
    expect(installSpec('1.0.0')).toBe(`${PACKAGE}@1.0.0`);
    expect(installSpec('1.0.0-beta.2')).toBe(`${PACKAGE}@1.0.0-beta.2`);
  });

  it('refuses anything a shell would act on', () => {
    for (const v of ['1.0.0; rm -rf ~', '1.0', 'latest', "1.0.0'", '$(x)']) {
      expect(() => installSpec(v), v).toThrow();
    }
    for (const o of ['/tmp/a b.tgz', '/tmp/a.tgz;x', "/tmp/'a.tgz", '$(x)']) {
      expect(() => installSpec('1.0.0', o), o).toThrow();
    }
  });

  it('takes a plain override, for tests', () => {
    expect(installSpec('1.0.0', '/tmp/jlongo78-agent-spaces-1.0.0.tgz')).toBe('/tmp/jlongo78-agent-spaces-1.0.0.tgz');
  });
});

describe('sshInstallArgs', () => {
  it('installs in the login shell, and restarts only for an update of a running server', () => {
    const fresh = sshInstallArgs('you@devbox', `${PACKAGE}@1.0.0`).at(-1);
    expect(fresh).toContain(`npm install -g ${PACKAGE}@1.0.0'`);
    expect(fresh).not.toContain('spaces restart');
    const update = sshInstallArgs('you@devbox', `${PACKAGE}@1.0.0`, { restart: true }).at(-1);
    expect(update).toContain(`npm install -g ${PACKAGE}@1.0.0 && spaces restart'`);
    expect(sshInstallArgs('you@devbox', 'x').slice(-3, -1)).toEqual(['--', 'you@devbox']);
  });
});

describe('classifyInstallFailure', () => {
  it('recognises the failures that have a fix to suggest', () => {
    expect(classifyInstallFailure('npm ERR! code EACCES\nnpm ERR! syscall mkdir')).toBe('permission');
    expect(classifyInstallFailure('npm ERR! code ETARGET\nnpm ERR! notarget No matching version found')).toBe('unpublished');
    expect(classifyInstallFailure('npm ERR! code E404')).toBe('unpublished');
    expect(classifyInstallFailure('gyp ERR! stack Error: not found: make')).toBe('build');
    expect(classifyInstallFailure('npm ERR! code EEXIST\nnpm ERR! File exists: /usr/bin/spaces')).toBe('conflict');
    expect(classifyInstallFailure('something else')).toBe('failed');
  });

  it('turns each into advice that names the machine', () => {
    for (const kind of ['permission', 'unpublished', 'build', 'conflict', 'failed']) {
      expect(installFailureHint(kind, 'you@devbox', `${PACKAGE}@1.0.0`)).toContain('you@devbox');
    }
  });
});

// The probe is shell code run on another machine; parsing it is not enough.
describe.skipIf(process.platform === 'win32')('the probe, run by a real shell', () => {
  const fs = require('fs');
  const os = require('os');
  const path = require('path');
  const { spawnSync } = require('child_process');

  function machine({ spaces }: { spaces: string | null }) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-probe-'));
    const put = (name: string, body: string) => fs.writeFileSync(path.join(dir, name), `#!/bin/sh\n${body}\n`, { mode: 0o755 });
    // Stands in for the login shell: runs the command it is given, nothing else.
    put('login-shell', 'exec /bin/sh -c "$2"');
    put('node', 'echo v22.1.0');
    if (spaces) {
      put('spaces', `case "$1" in --version) echo ${spaces};; status) echo SPACES_RUNNING=1; echo SPACES_LIVE_AGENTS=2;; esac`);
    }
    return dir;
  }
  function probe(dir: string) {
    const r = spawnSync('/bin/sh', ['-c', sshProbeArgs('h').at(-1)!], {
      encoding: 'utf-8',
      env: { PATH: `${dir}:/usr/bin:/bin`, HOME: dir, SHELL: path.join(dir, 'login-shell') },
    });
    expect(r.stderr).toBe('');
    return parseProbe(r.stdout);
  }

  it('reports a current install with its agents', () => {
    expect(probe(machine({ spaces: '1.2.3' })))
      .toEqual({ node: '22.1.0', missing: false, version: '1.2.3', running: true, liveAgents: 2 });
  });

  it('does not ask an old install for a status it does not have', () => {
    expect(probe(machine({ spaces: '0.9.10' })))
      .toEqual({ node: '22.1.0', missing: false, version: '0.9.10', running: null, liveAgents: null });
  });

  it('reports a machine without Spaces', () => {
    expect(probe(machine({ spaces: null }))).toMatchObject({ node: '22.1.0', missing: true, version: null });
  });
});
