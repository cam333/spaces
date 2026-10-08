import { describe, it, expect } from 'vitest';
import {
  experimentalEnabled,
  multiuserRequest,
  gateMultiuser,
  gateFederation,
  FEDERATION_WHOAMI_NOTE,
} from '../../bin/lib/experimental.js';

describe('experimentalEnabled', () => {
  it('is off by default', () => {
    expect(experimentalEnabled({ flag: undefined, env: {} })).toBe(false);
    expect(experimentalEnabled()).toBe(false);
  });

  it('is on with the CLI flag', () => {
    expect(experimentalEnabled({ flag: true, env: {} })).toBe(true);
  });

  it('is on with SPACES_EXPERIMENTAL=1 (or true)', () => {
    expect(experimentalEnabled({ env: { SPACES_EXPERIMENTAL: '1' } })).toBe(true);
    expect(experimentalEnabled({ env: { SPACES_EXPERIMENTAL: 'true' } })).toBe(true);
  });

  it('ignores other values of SPACES_EXPERIMENTAL', () => {
    for (const v of ['', '0', 'false', 'no', 'yes please']) {
      expect(experimentalEnabled({ env: { SPACES_EXPERIMENTAL: v } })).toBe(false);
    }
  });

  it('takes no saved setting: only the flag and the environment count', () => {
    // The signature has no `saved` input at all; passing one changes nothing.
    expect(experimentalEnabled({ env: {}, saved: { experimental: true } } as never)).toBe(false);
  });
});

describe('multiuserRequest', () => {
  it('defaults to desktop mode', () => {
    expect(multiuserRequest({ env: {}, saved: {} })).toEqual({ multiuser: false, source: null });
  });

  it('names where a multiuser request came from', () => {
    expect(multiuserRequest({ flag: true }).source).toMatch(/--multiuser/);
    expect(multiuserRequest({ env: { SPACES_MULTIUSER: '1' } }).source).toMatch(/SPACES_MULTIUSER/);
    expect(multiuserRequest({ saved: { multiuser: true } }).source).toMatch(/server\.json/);
  });

  it('lets --desktop (flag false) override the environment and a saved setting', () => {
    expect(
      multiuserRequest({ flag: false, env: { SPACES_MULTIUSER: '1' }, saved: { multiuser: true } }),
    ).toEqual({ multiuser: false, source: null });
  });
});

describe('gateMultiuser', () => {
  it('allows desktop mode with or without the opt-in', () => {
    expect(gateMultiuser({ multiuser: false, source: null, experimental: false })).toEqual({ allowed: true });
    expect(gateMultiuser({ multiuser: false, source: null, experimental: true })).toEqual({ allowed: true });
  });

  it('allows multiuser only with the opt-in', () => {
    expect(gateMultiuser({ multiuser: true, source: 'x', experimental: true })).toEqual({ allowed: true });
  });

  it('refuses multiuser without the opt-in, explaining the risk and the way out', () => {
    const verdict = gateMultiuser({
      multiuser: true,
      source: '"multiuser": true in ~/.spaces/server.json',
      experimental: false,
    });
    expect(verdict.allowed).toBe(false);
    const text = (verdict.lines ?? []).join('\n');
    expect(text).toContain('server.json');
    expect(text).toMatch(/experimental/);
    expect(text).toMatch(/gain control/);
    expect(text).toContain('--desktop');
    expect(text).toContain('--experimental');
    expect(text).toContain('SPACES_EXPERIMENTAL=1');
  });

  it('end to end: a saved multiuser setting is refused, and --desktop still starts', () => {
    const saved = { multiuser: true };
    const refused = multiuserRequest({ flag: undefined, env: {}, saved });
    expect(gateMultiuser({ ...refused, experimental: experimentalEnabled({ env: {} }) }).allowed).toBe(false);

    const desktop = multiuserRequest({ flag: false, env: {}, saved });
    expect(gateMultiuser({ ...desktop, experimental: false }).allowed).toBe(true);
  });
});

describe('gateFederation', () => {
  it('does nothing when federation was not asked for', () => {
    expect(gateFederation({ iface: undefined, experimental: false })).toEqual({ start: false });
    expect(gateFederation({ iface: '', experimental: true })).toEqual({ start: false });
  });

  it('starts with the opt-in', () => {
    expect(gateFederation({ iface: 'tailscale0', experimental: true })).toEqual({ start: true });
  });

  it('warns and does not start without the opt-in', () => {
    const verdict = gateFederation({ iface: 'tailscale0', experimental: false });
    expect(verdict.start).toBe(false);
    const text = (verdict.lines ?? []).join('\n');
    expect(text).toContain('tailscale0');
    expect(text).toMatch(/experimental/);
    expect(text).toMatch(/act as you/);
    expect(text).toMatch(/not encrypted/);
  });

  it('has a whoami note that says the same', () => {
    const text = FEDERATION_WHOAMI_NOTE.join('\n');
    expect(text).toMatch(/experimental/);
    expect(text).toMatch(/not encrypted/);
  });
});
