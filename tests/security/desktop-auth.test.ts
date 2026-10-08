import { describe, it, expect } from 'vitest';
import {
  isLocalHost,
  signDesktop,
  verifyDesktop,
  createLoginCodes,
  desktopCookie,
  presentedSecret,
  secretMatches,
  DESKTOP_COOKIE,
} from '../../bin/lib/desktop-auth.js';

const SECRET = 'a'.repeat(64);
const OTHER = 'b'.repeat(64);

describe('isLocalHost', () => {
  it('accepts the loopback names on any port', () => {
    for (const h of ['localhost', 'localhost:3457', '127.0.0.1', '127.0.0.1:41999', '[::1]', '[::1]:3457', 'LOCALHOST:1']) {
      expect(isLocalHost(h)).toBe(true);
    }
  });

  it('refuses every other name, which is what DNS rebinding sends', () => {
    for (const h of ['attacker.example:3457', 'localhost.attacker.example', '127.0.0.1.nip.io', '10.0.0.5:3457', '', undefined, 'localhost:abc', 'localhost:3457:1']) {
      expect(isLocalHost(h as any)).toBe(false);
    }
  });

  it('accepts names the operator configured', () => {
    expect(isLocalHost('spaces.home.arpa:3457', ['spaces.home.arpa'])).toBe(true);
    expect(isLocalHost('other.home.arpa', ['spaces.home.arpa'])).toBe(false);
  });
});

describe('signed desktop tokens', () => {
  it('verify with the same secret and kind', () => {
    const t = signDesktop(SECRET, 'session', 60, 1000);
    expect(verifyDesktop(SECRET, t, 'session', 1000)).toMatchObject({ k: 'session' });
  });

  it('do not verify with another secret, another kind, or after expiry', () => {
    const t = signDesktop(SECRET, 'session', 60, 1000);
    expect(verifyDesktop(OTHER, t, 'session', 1000)).toBeNull();
    expect(verifyDesktop(SECRET, t, 'login', 1000)).toBeNull();
    expect(verifyDesktop(SECRET, t, 'session', 1061)).toBeNull();
  });

  it('do not verify when tampered with or malformed', () => {
    const t = signDesktop(SECRET, 'session', 60, 1000);
    const [payload, sig] = t.split('.');
    const forged = Buffer.from(JSON.stringify({ k: 'session', exp: 9e9, n: 'x' })).toString('base64url');
    expect(verifyDesktop(SECRET, `${forged}.${sig}`, 'session', 1000)).toBeNull();
    expect(verifyDesktop(SECRET, `${payload}.`, 'session', 1000)).toBeNull();
    for (const junk of ['', 'a', 'a.b.c', '...', undefined]) {
      expect(verifyDesktop(SECRET, junk as any, 'session', 1000)).toBeNull();
    }
  });

  it('refuse a missing or short secret outright', () => {
    expect(() => signDesktop('', 'session', 60, 0)).toThrow();
    expect(verifyDesktop('', 'x.y', 'session', 0)).toBeNull();
  });
});

describe('login codes', () => {
  it('work once, then never again', () => {
    const codes = createLoginCodes(() => SECRET, () => 1000);
    const code = codes.mint();
    expect(codes.redeem(code)).toBe(true);
    expect(codes.redeem(code)).toBe(false);
  });

  it('expire after two minutes', () => {
    let now = 1000;
    const codes = createLoginCodes(() => SECRET, () => now);
    const code = codes.mint();
    now += 121;
    expect(codes.redeem(code)).toBe(false);
  });

  it('cannot be redeemed as a session, nor a session as a code', () => {
    const codes = createLoginCodes(() => SECRET, () => 1000);
    expect(verifyDesktop(SECRET, codes.mint(), 'session', 1000)).toBeNull();
    expect(codes.redeem(signDesktop(SECRET, 'session', 60, 1000))).toBe(false);
  });

  it('minted by another process holding the same secret are accepted', () => {
    const cli = createLoginCodes(() => SECRET, () => 1000);
    const server = createLoginCodes(() => SECRET, () => 1001);
    expect(server.redeem(cli.mint())).toBe(true);
  });
});

describe('desktopCookie', () => {
  it('cannot be read by scripts or sent by other sites', () => {
    const c = desktopCookie('v', '');
    expect(c).toContain(`${DESKTOP_COOKIE}=v`);
    expect(c).toContain('HttpOnly');
    expect(c).toContain('SameSite=Strict');
    expect(c).toContain('Path=/');
  });

  it('is scoped to the base path', () => {
    expect(desktopCookie('v', '/spaces')).toContain('Path=/spaces');
  });
});

describe('presentedSecret / secretMatches', () => {
  it('reads a bearer or the internal header, never the query string', () => {
    expect(presentedSecret({ authorization: `Bearer ${SECRET}` })).toBe(SECRET);
    expect(presentedSecret({ 'x-spaces-internal': SECRET })).toBe(SECRET);
    expect(presentedSecret({ authorization: 'Basic abc' })).toBe('');
    expect(presentedSecret({})).toBe('');
  });

  it('compares in constant time and refuses empty values', () => {
    expect(secretMatches(SECRET, SECRET)).toBe(true);
    expect(secretMatches(SECRET, OTHER)).toBe(false);
    expect(secretMatches(SECRET, SECRET.slice(1))).toBe(false);
    expect(secretMatches('', '')).toBe(false);
    expect(secretMatches(SECRET, '')).toBe(false);
  });
});

describe('the second round of hardening', () => {
  it('accepts allowed hosts written as full origins', async () => {
    const { isLocalHost: h } = await import('../../bin/lib/desktop-auth.js');
    expect(h('spaces.example.com', ['https://spaces.example.com:8443'])).toBe(true);
    expect(h('spaces.example.com:8443', ['spaces.example.com'])).toBe(true);
    expect(h('evil.example.com', ['https://spaces.example.com'])).toBe(false);
  });

  it('treats only this exact origin as self, not another port on localhost', async () => {
    const { originIsSelf } = await import('../../bin/lib/desktop-auth.js');
    expect(originIsSelf('http://localhost:3457', 'localhost:3457')).toBe(true);
    expect(originIsSelf('https://localhost:3457', 'localhost:3457')).toBe(true);
    expect(originIsSelf('http://localhost:8080', 'localhost:3457')).toBe(false);
    expect(originIsSelf('http://127.0.0.1:3457', 'localhost:3457')).toBe(false);
    expect(originIsSelf('null', 'localhost:3457')).toBe(false);
  });

  it('sends tools a short-lived token, and only to the loopback', async () => {
    const { clientAuthHeaders, verifyDesktop, API_TTL_S } = await import('../../bin/lib/desktop-auth.js');
    const h = clientAuthHeaders('http://localhost:3457', SECRET);
    const token = h.Authorization.replace('Bearer ', '');
    expect(token).not.toContain(SECRET);
    const payload = verifyDesktop(SECRET, token, 'api');
    expect(payload.exp - Math.floor(Date.now() / 1000)).toBeLessThanOrEqual(API_TTL_S);
    expect(clientAuthHeaders('https://evil.example', SECRET)).toEqual({});
    expect(clientAuthHeaders('http://localhost.evil.example:3457', SECRET)).toEqual({});
    expect(clientAuthHeaders('http://localhost:3457', '')).toEqual({});
  });

  it('counts a session or per-request token as the owner, but never a login code', async () => {
    const { isOwnerCredential } = await import('../../bin/lib/desktop-auth.js');
    expect(isOwnerCredential(SECRET, SECRET)).toBe(true);
    expect(isOwnerCredential(SECRET, signDesktop(SECRET, 'session', 60))).toBe(true);
    expect(isOwnerCredential(SECRET, signDesktop(SECRET, 'api', 60))).toBe(true);
    expect(isOwnerCredential(SECRET, signDesktop(SECRET, 'login', 60))).toBe(false);
    expect(isOwnerCredential(SECRET, '')).toBe(false);
  });
});
