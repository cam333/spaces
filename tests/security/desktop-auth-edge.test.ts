import { describe, it, expect } from 'vitest';
import { signDesktop, isLocalHost as nodeIsLocalHost } from '../../bin/lib/desktop-auth.js';
import { verifyDesktopSession, isLocalHost, presentedSecret, timingSafeEqualStr } from '@/lib/auth/desktop';

const SECRET = 'c'.repeat(64);

describe('the middleware verifies what the server signs', () => {
  it('accepts a session the Node side signed', async () => {
    expect(await verifyDesktopSession(SECRET, signDesktop(SECRET, 'session', 60))).toBe(true);
  });

  it('refuses a login code, an expired session, another secret and junk', async () => {
    expect(await verifyDesktopSession(SECRET, signDesktop(SECRET, 'login', 60))).toBe(false);
    expect(await verifyDesktopSession(SECRET, signDesktop(SECRET, 'session', -10))).toBe(false);
    expect(await verifyDesktopSession('d'.repeat(64), signDesktop(SECRET, 'session', 60))).toBe(false);
    for (const junk of [undefined, '', 'x', 'a.b', 'a.b.c']) {
      expect(await verifyDesktopSession(SECRET, junk)).toBe(false);
    }
    expect(await verifyDesktopSession('', signDesktop(SECRET, 'session', 60))).toBe(false);
  });

  it('applies the same Host rule as the Node side', () => {
    for (const h of ['localhost:3457', '127.0.0.1', '[::1]:1', 'evil.example:3457', 'localhost.evil.example', '']) {
      expect(isLocalHost(h)).toBe(nodeIsLocalHost(h));
    }
  });

  it('reads the secret from headers only', () => {
    expect(presentedSecret(new Headers({ authorization: `Bearer ${SECRET}` }))).toBe(SECRET);
    expect(presentedSecret(new Headers({ 'x-spaces-internal': SECRET }))).toBe(SECRET);
    expect(presentedSecret(new Headers())).toBe('');
  });

  it('never matches empty strings', () => {
    expect(timingSafeEqualStr('', '')).toBe(false);
    expect(timingSafeEqualStr(SECRET, SECRET)).toBe(true);
  });
});

describe('the middleware, second round', () => {
  it('accepts per-request and session tokens as bearers, never a login code or a short secret', async () => {
    const { isOwnerCredential } = await import('@/lib/auth/desktop');
    expect(await isOwnerCredential(SECRET, signDesktop(SECRET, 'api', 60))).toBe(true);
    expect(await isOwnerCredential(SECRET, signDesktop(SECRET, 'session', 60))).toBe(true);
    expect(await isOwnerCredential(SECRET, signDesktop(SECRET, 'login', 60))).toBe(false);
    expect(await isOwnerCredential('short', 'short')).toBe(false);
  });

  it('lets the cookie count only for requests from Spaces itself', async () => {
    const { cameFromSelf } = await import('@/lib/auth/desktop');
    const h = (o: Record<string, string>) => new Headers({ host: 'localhost:3457', ...o });
    expect(cameFromSelf(h({ 'sec-fetch-site': 'same-origin' }))).toBe(true);
    expect(cameFromSelf(h({ 'sec-fetch-site': 'none' }))).toBe(true);
    expect(cameFromSelf(h({ 'sec-fetch-site': 'same-site', origin: 'http://localhost:8080' }))).toBe(false);
    expect(cameFromSelf(h({ 'sec-fetch-site': 'cross-site' }))).toBe(false);
    expect(cameFromSelf(h({ origin: 'http://localhost:8080' }))).toBe(false);
    expect(cameFromSelf(h({ origin: 'http://localhost:3457' }))).toBe(true);
    expect(cameFromSelf(h({}))).toBe(true);
  });
});
