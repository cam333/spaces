import { describe, it, expect, afterEach, vi } from 'vitest';

// getAuthUserOrNull must NOT fall back to the OS user in multiuser mode — that
// fallback let unauthenticated callers act as the server account (review H2).
// flags.ts reads env at import time, so we reset modules + stub env per case.

afterEach(() => {
  vi.unstubAllEnvs();
  vi.resetModules();
});

describe('getAuthUserOrNull', () => {
  it('returns null in multiuser mode when x-auth-user is absent', async () => {
    vi.stubEnv('SPACES_MULTIUSER', '1');
    vi.resetModules();
    const { getAuthUserOrNull } = await import('@/lib/auth/context');
    const req = new Request('http://x/');
    expect(getAuthUserOrNull(req)).toBeNull();
  });

  it('returns the header value in multiuser mode when present', async () => {
    vi.stubEnv('SPACES_MULTIUSER', '1');
    vi.resetModules();
    const { getAuthUserOrNull } = await import('@/lib/auth/context');
    const req = new Request('http://x/', { headers: { 'x-auth-user': 'alice' } });
    expect(getAuthUserOrNull(req)).toBe('alice');
  });

  it('falls back to the OS user only in desktop mode', async () => {
    vi.stubEnv('SPACES_MULTIUSER', '');
    vi.stubEnv('NEXT_PUBLIC_MULTIUSER', '');
    vi.resetModules();
    const os = await import('os');
    const { getAuthUserOrNull } = await import('@/lib/auth/context');
    const req = new Request('http://x/');
    expect(getAuthUserOrNull(req)).toBe(os.userInfo().username);
  });
});
