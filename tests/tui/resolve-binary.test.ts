import { describe, it, expect, vi } from 'vitest';
import * as fs from 'fs';
import * as net from 'net';
import * as os from 'os';
import * as path from 'path';
import {
  platformPackageName,
  binaryFileName,
  noBinaryMessage,
  resolveTuiBinary,
  splitToken,
  buildTuiArgs,
  portIsListening,
  builtTuiBinary,
  splitServerFlags,
  resolveMultiuser,
  waitForServer,
  tailLines,
  TARGETS,
} from '../../bin/spaces-tui.js';

describe('platformPackageName', () => {
  it('maps the five supported targets', () => {
    expect(platformPackageName('linux', 'x64')).toBe('@jlongo78/agent-spaces-tui-linux-x64');
    expect(platformPackageName('linux', 'arm64')).toBe('@jlongo78/agent-spaces-tui-linux-arm64');
    expect(platformPackageName('darwin', 'x64')).toBe('@jlongo78/agent-spaces-tui-darwin-x64');
    expect(platformPackageName('darwin', 'arm64')).toBe('@jlongo78/agent-spaces-tui-darwin-arm64');
    expect(platformPackageName('win32', 'x64')).toBe('@jlongo78/agent-spaces-tui-win32-x64');
  });

  it('returns null for an unsupported target rather than guessing', () => {
    expect(platformPackageName('sunos', 'sparc')).toBeNull();
    expect(platformPackageName('win32', 'ia32')).toBeNull();
  });
});

describe('binaryFileName', () => {
  it('adds .exe only on windows', () => {
    expect(binaryFileName('win32')).toBe('spaces-tui.exe');
    expect(binaryFileName('linux')).toBe('spaces-tui');
    expect(binaryFileName('darwin')).toBe('spaces-tui');
  });
});

describe('resolveTuiBinary', () => {
  // resolveTuiBinary has three ways to end up with "no binary available,"
  // and each must degrade to null rather than throw — the caller falls
  // back to the browser flow. All three are exercised deterministically
  // via the optional platform/arch/resolve parameters (mirroring the
  // platform/arch defaults already on platformPackageName/binaryFileName),
  // rather than relying on the ambient state of this dev checkout.

  it('never throws and returns null when the platform package is not installed', () => {
    // Driven through the injectable resolver, like its two siblings.
    // Asserting against the *ambient* checkout (the original form of this
    // case) encoded "nothing has been published to npm yet" as a
    // requirement: it would start failing the day the five platform
    // packages exist and get installed, which is precisely when this code
    // path matters most.
    const resolve = () => {
      throw new Error("Cannot find module '@jlongo78/agent-spaces-tui-linux-x64/package.json'");
    };
    expect(() => resolveTuiBinary('linux', 'x64', resolve)).not.toThrow();
    expect(resolveTuiBinary('linux', 'x64', resolve)).toBeNull();
  });

  it('returns null immediately for an unsupported platform, without attempting to resolve anything', () => {
    const resolve = vi.fn();
    expect(() => resolveTuiBinary('sunos', 'sparc', resolve)).not.toThrow();
    expect(resolveTuiBinary('sunos', 'sparc', resolve)).toBeNull();
    expect(resolve).not.toHaveBeenCalled();
  });

  it('returns null when the package resolves but its bin/ file is missing', () => {
    // Simulates a broken `files` field or an incomplete build in one of
    // the five platform packages — the failure most likely to actually
    // happen in this distribution mechanism. A real temp directory holds
    // a package.json with no bin/ file; an injected resolver points at
    // it so the rest of resolveTuiBinary's real path.dirname/join/
    // existsSync logic runs unmocked.
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agent-spaces-tui-test-'));
    const pkgJsonPath = path.join(tmpDir, 'package.json');
    fs.writeFileSync(pkgJsonPath, '{}');
    try {
      const resolve = () => pkgJsonPath;
      expect(() => resolveTuiBinary('linux', 'x64', resolve)).not.toThrow();
      expect(resolveTuiBinary('linux', 'x64', resolve)).toBeNull();
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it('returns the binary path when the package resolves and the file is there', () => {
    // The success branch — never exercised before, because in a dev
    // checkout the real resolver always threw.
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agent-spaces-tui-test-'));
    const pkgJsonPath = path.join(tmpDir, 'package.json');
    fs.writeFileSync(pkgJsonPath, '{}');
    fs.mkdirSync(path.join(tmpDir, 'bin'));
    const binPath = path.join(tmpDir, 'bin', 'spaces-tui');
    fs.writeFileSync(binPath, '#!/bin/sh\n');
    // A root with no `tui/` — otherwise this suite, which runs inside a
    // checkout that has a built binary, cannot tell "found nothing" from
    // "fell back to the repo's own build".
    const noBuild = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-nobuild-'));
    try {
      expect(resolveTuiBinary('linux', 'x64', () => pkgJsonPath, noBuild)).toBe(binPath);
      // The Windows package holds spaces-tui.exe, which isn't there.
      expect(resolveTuiBinary('win32', 'x64', () => pkgJsonPath, noBuild)).toBeNull();
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});

describe('noBinaryMessage', () => {
  it('names the platform and lists every supported target', () => {
    const message = noBinaryMessage('sunos', 'sparc');
    expect(message).toContain('sunos-sparc');
    for (const target of Object.keys(TARGETS)) {
      expect(message).toContain(target);
    }
  });

  it('on a supported platform, says the optional package was skipped and how to fix it', () => {
    const message = noBinaryMessage('linux', 'x64');
    expect(message).toContain('@jlongo78/agent-spaces-tui-linux-x64');
    expect(message).toContain('not installed');
    expect(message).toContain('--omit=optional');
    expect(message).not.toContain('Supported:');
  });
});

describe('splitServerFlags', () => {
  // `spaces tui --desktop` used to forward `--desktop` to the client, which
  // knows only --url/--token and exited with "unrecognized argument".

  it('separates the server flags from the client flags', () => {
    expect(splitServerFlags(['--port', '4000', '--desktop', '--token', 't'])).toEqual({
      serverArgs: ['--port', '4000', '--desktop'],
      clientArgs: ['--token', 't'],
    });
  });

  it('recognises every spelling of the mode flags', () => {
    for (const flag of ['--desktop', '--no-multiuser', '--multiuser']) {
      expect(splitServerFlags([flag]).serverArgs).toEqual([flag]);
    }
  });

  it('leaves a valueless trailing --port in the client args for it to reject', () => {
    expect(splitServerFlags(['--port'])).toEqual({ serverArgs: [], clientArgs: ['--port'] });
  });

  it('passes client flags through untouched and in order', () => {
    expect(splitServerFlags(['--url', 'ws://h:1', '--token', 'x']).clientArgs)
      .toEqual(['--url', 'ws://h:1', '--token', 'x']);
  });
});

describe('waitForServer', () => {
  // A background server that dies on startup ("No build found") used to be
  // waited on for the full timeout, then reported only as "did not come up".

  it('reports up as soon as the port answers', () => {
    let probes = 0;
    const r = waitForServer({ timeoutMs: 10_000, probe: () => ++probes === 3, alive: () => true });
    expect(r).toBe('up');
    expect(probes).toBe(3);
  });

  it('stops waiting the moment the server process has exited', () => {
    let probes = 0;
    const r = waitForServer({ timeoutMs: 10_000, probe: () => { probes++; return false; }, alive: () => probes < 2 });
    expect(r).toBe('exited');
    expect(probes).toBe(2);
  });

  it('times out when the process lives but never listens', () => {
    let now = 0;
    const r = waitForServer({ timeoutMs: 3000, probe: () => { now += 1000; return false; }, alive: () => true, clock: () => now });
    expect(r).toBe('timeout');
  });
});

describe('tailLines', () => {
  it('keeps the last non-blank lines', () => {
    expect(tailLines('a\n\nb\nc\n\n', 2)).toEqual(['b', 'c']);
    expect(tailLines('', 5)).toEqual([]);
  });
});

describe('resolveMultiuser', () => {
  // Same precedence `startServer()` applies: an explicit flag, then the
  // environment, then the saved config. The launcher needs the answer before
  // starting anything, because a multiuser server refuses /proto outright.

  it('defaults to desktop mode', () => {
    expect(resolveMultiuser({})).toBe(false);
  });

  it('honours the saved config', () => {
    expect(resolveMultiuser({ saved: { multiuser: true } })).toBe(true);
  });

  it('honours SPACES_MULTIUSER=1', () => {
    expect(resolveMultiuser({ env: { SPACES_MULTIUSER: '1' } })).toBe(true);
  });

  it('lets an explicit flag win in both directions', () => {
    expect(resolveMultiuser({ flag: false, env: { SPACES_MULTIUSER: '1' }, saved: { multiuser: true } })).toBe(false);
    expect(resolveMultiuser({ flag: true })).toBe(true);
  });
});

describe('splitToken', () => {
  // The terminal client prefers SPACES_TOKEN from its environment; the
  // launcher moves --token there so the secret does not land on a third
  // process command line (the two spaces.js processes already have it).

  it('lifts --token out of argv and returns it separately', () => {
    expect(splitToken(['--url', 'ws://h:1', '--token', 's3cret'])).toEqual({
      token: 's3cret',
      rest: ['--url', 'ws://h:1'],
    });
  });

  it('leaves argv alone when no token is present', () => {
    expect(splitToken(['--url', 'ws://h:1'])).toEqual({
      token: null,
      rest: ['--url', 'ws://h:1'],
    });
  });

  it('takes the last --token when it is repeated', () => {
    expect(splitToken(['--token', 'a', '--token', 'b']).token).toBe('b');
  });

  it('leaves a valueless trailing --token in place for the client to reject', () => {
    // Swallowing it here would turn the client's clear "--token requires a
    // value" into a silent, tokenless connection attempt.
    expect(splitToken(['--url', 'ws://h:1', '--token'])).toEqual({
      token: null,
      rest: ['--url', 'ws://h:1', '--token'],
    });
  });

  it('does not treat a --token value that looks like a flag as a flag', () => {
    expect(splitToken(['--token', '--url'])).toEqual({ token: '--url', rest: [] });
  });
});

describe('buildTuiArgs', () => {
  // --url is mandatory in the client and has no default; both `spaces tui`
  // and bare `spaces` used to spawn it with no arguments at all, so the
  // moment the platform binary installs, the primary command exits 2 with
  // a usage error instead of starting.

  it('supplies a local --url when the caller gave none', () => {
    expect(buildTuiArgs([], 3457)).toEqual(['--url', 'ws://127.0.0.1:3457']);
  });

  it('uses the resolved port, not a hardcoded one', () => {
    expect(buildTuiArgs([], 3999)).toEqual(['--url', 'ws://127.0.0.1:3999']);
  });

  it('keeps any other arguments the caller passed', () => {
    expect(buildTuiArgs(['--token', 'x'], 3457)).toEqual([
      '--url',
      'ws://127.0.0.1:3457',
      '--token',
      'x',
    ]);
  });

  it('leaves an explicit --url untouched', () => {
    const argv = ['--url', 'ws://elsewhere:9000'];
    expect(buildTuiArgs(argv, 3457)).toEqual(argv);
  });

  it('does not mutate the array it was given', () => {
    const argv: string[] = [];
    buildTuiArgs(argv, 3457);
    expect(argv).toEqual([]);
  });
});

describe('portIsListening', () => {
  // The launcher asks "is something listening?", which is not what a pid
  // lookup answers. `findPidsOnPort` shells out to lsof/fuser, absent from
  // most slim Linux images, where it silently reports nothing — so the
  // launcher believed every port was free, spawned a duplicate server on
  // every invocation, and then hung for the full startup timeout polling
  // the same broken probe. This connects instead.

  /** A throwaway TCP listener on an ephemeral port. Not a Spaces server. */
  async function listener(): Promise<{ port: number; close: () => Promise<void> }> {
    const server = net.createServer((socket) => socket.end());
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (typeof address === 'string' || address === null) throw new Error('no port');
    return {
      port: address.port,
      close: () => new Promise<void>((resolve) => server.close(() => resolve())),
    };
  }

  it('reports a listening port as listening', async () => {
    const { port, close } = await listener();
    try {
      expect(portIsListening(port)).toBe(true);
    } finally {
      await close();
    }
  });

  it('reports a closed port as not listening', async () => {
    // Bind then release, so the port is known to have had nothing on it
    // rather than being a guess that might collide with a real service.
    const { port, close } = await listener();
    await close();
    expect(portIsListening(port)).toBe(false);
  });

  it('refuses a malformed port outright instead of probing its leading digits', async () => {
    // Every value below embeds the port of a listener that IS up. A guard
    // that parses a leading digit prefix and discards the rest — which
    // Number.parseInt does — would connect to it and answer true. Only a
    // guard that refuses the whole value answers false.
    //
    // Using a live canary rather than a presumed-closed port is what makes
    // this deterministic: the previous version of this case asserted
    // `portIsListening('3457; rm -rf /') === false` and passed only
    // because 3457 happened to be closed, so it flipped to a failure on
    // any machine running spaces on its default port while running the
    // suite.
    const { port, close } = await listener();
    try {
      expect(portIsListening(port)).toBe(true); // the canary really is up

      const malformed: unknown[] = [
        `${port}; rm -rf /`, // trailing garbage
        `${port}.9`, // float as a string
        port + 0.9, // float as a number
        `${port} `, // trailing whitespace
        ` ${port}`, // leading whitespace
        `0x${port.toString(16)}`, // hex
        [port, 66], // non-primitive whose String() starts with digits
      ];
      for (const bad of malformed) {
        expect(
          portIsListening(bad as never),
          `${JSON.stringify(bad)} must be refused, not truncated to ${port}`,
        ).toBe(false);
      }
    } finally {
      await close();
    }
  });

  it('refuses out-of-range and non-values', () => {
    // These carry no valid port to truncate to, so they need no canary —
    // nothing can be listening on any of them under any parse.
    for (const bad of ['0', '70000', '-1', 'not-a-port', '', null, undefined, NaN, {}]) {
      expect(portIsListening(bad as never)).toBe(false);
    }
  });

  it('returns as soon as the port answers rather than burning the whole wait', async () => {
    const { port, close } = await listener();
    try {
      const startedAt = Date.now();
      expect(portIsListening(port, 30000)).toBe(true);
      expect(Date.now() - startedAt).toBeLessThan(10000);
    } finally {
      await close();
    }
  });
});

describe('a binary built from a checkout', () => {
  function tree(profiles: string[], platform = 'linux') {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-built-'));
    for (const profile of profiles) {
      const dir = path.join(root, 'tui', 'target', profile);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, platform === 'win32' ? 'spaces-tui.exe' : 'spaces-tui'), '');
    }
    return root;
  }

  it('is found in a development tree', () => {
    // Without this the primary command could not find the binary sitting two
    // directories away that the developer had just built: it reported the
    // platform as unsupported and silently opened a browser.
    const root = tree(['debug']);
    expect(builtTuiBinary('linux', root)).toBe(
      path.join(root, 'tui', 'target', 'debug', 'spaces-tui')
    );
  });

  it('prefers release over debug when both exist', () => {
    const root = tree(['debug', 'release']);
    expect(builtTuiBinary('linux', root)).toContain(path.join('target', 'release'));
  });

  it('uses the platform executable name', () => {
    const root = tree(['debug'], 'win32');
    expect(builtTuiBinary('win32', root)).toContain('spaces-tui.exe');
  });

  it('finds nothing in an installed package, which has no tui directory', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-installed-'));
    expect(builtTuiBinary('linux', root)).toBeNull();
  });

  it('never shadows an installed package', () => {
    // The published package still wins wherever there is one, so this changes
    // nothing for anyone who did not build from source. Checked against this
    // very repo, which has a built binary present.
    const viaPackage = resolveTuiBinary(process.platform, process.arch, (id: string) => {
      if (String(id).includes('agent-spaces-tui-')) return require.resolve('vitest/package.json');
      throw new Error('not installed');
    });
    // vitest has no bin/spaces-tui, so resolution falls through to the built
    // one — the point being that the package branch ran first and was tried.
    expect(viaPackage).toBe(builtTuiBinary(process.platform));
  });
});
