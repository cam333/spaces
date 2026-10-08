import { describe, it, expect, afterEach, vi } from 'vitest';

// session.ts caches the secret + reads flags at import time, so reset modules
// and stub a deterministic hex secret per case.
afterEach(() => {
  vi.unstubAllEnvs();
  vi.resetModules();
});

const SECRET = 'a'.repeat(64); // 32 bytes of hex

describe('session token versioning (M7)', () => {
  it('round-trips sub/role/ver', async () => {
    vi.stubEnv('SPACES_SESSION_SECRET', SECRET);
    vi.resetModules();
    const { issueSessionToken, verifySessionToken } = await import('@/lib/auth/session');
    const tok = issueSessionToken('alice', 'admin', 7);
    expect(verifySessionToken(tok)).toEqual({ sub: 'alice', role: 'admin', ver: 7 });
  });

  it('defaults ver to 0 when issued without one', async () => {
    vi.stubEnv('SPACES_SESSION_SECRET', SECRET);
    vi.resetModules();
    const { issueSessionToken, verifySessionToken } = await import('@/lib/auth/session');
    expect(verifySessionToken(issueSessionToken('bob', 'user'))?.ver).toBe(0);
  });

  it('rejects tampered or malformed tokens (no length-mismatch throw)', async () => {
    vi.stubEnv('SPACES_SESSION_SECRET', SECRET);
    vi.resetModules();
    const { issueSessionToken, verifySessionToken } = await import('@/lib/auth/session');
    const tok = issueSessionToken('alice', 'user', 1);
    expect(verifySessionToken(tok + 'x')).toBeNull();
    expect(verifySessionToken('garbage')).toBeNull();
    expect(verifySessionToken('')).toBeNull();
  });
});
