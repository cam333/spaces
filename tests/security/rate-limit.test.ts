import { describe, it, expect } from 'vitest';
import {
  memoryStore,
  checkRateLimit,
  recordFailure,
  recordSuccess,
  RATE_LIMIT,
} from '@/lib/auth/rate-limit';

describe('auth rate limiter', () => {
  it('allows attempts under the threshold', () => {
    const store = memoryStore();
    const now = 1_000_000;
    for (let i = 0; i < RATE_LIMIT.maxFails - 1; i++) recordFailure(store, 'user:alice', now);
    expect(checkRateLimit(store, 'user:alice', now).allowed).toBe(true);
  });

  it('locks out after maxFails and reports retryAfter', () => {
    const store = memoryStore();
    const now = 1_000_000;
    for (let i = 0; i < RATE_LIMIT.maxFails; i++) recordFailure(store, 'user:alice', now);
    const res = checkRateLimit(store, 'user:alice', now);
    expect(res.allowed).toBe(false);
    expect(res.retryAfterMs).toBeGreaterThan(0);
  });

  it('unlocks after the lockout window elapses', () => {
    const store = memoryStore();
    const now = 1_000_000;
    for (let i = 0; i < RATE_LIMIT.maxFails; i++) recordFailure(store, 'user:alice', now);
    expect(checkRateLimit(store, 'user:alice', now).allowed).toBe(false);
    const later = now + RATE_LIMIT.lockMs + 1;
    expect(checkRateLimit(store, 'user:alice', later).allowed).toBe(true);
  });

  it('resets on success', () => {
    const store = memoryStore();
    const now = 1_000_000;
    for (let i = 0; i < RATE_LIMIT.maxFails; i++) recordFailure(store, 'user:alice', now);
    expect(checkRateLimit(store, 'user:alice', now).allowed).toBe(false);
    recordSuccess(store, 'user:alice');
    expect(checkRateLimit(store, 'user:alice', now).allowed).toBe(true);
  });

  it('tracks keys independently', () => {
    const store = memoryStore();
    const now = 1_000_000;
    for (let i = 0; i < RATE_LIMIT.maxFails; i++) recordFailure(store, 'user:alice', now);
    expect(checkRateLimit(store, 'user:alice', now).allowed).toBe(false);
    expect(checkRateLimit(store, 'user:bob', now).allowed).toBe(true);
  });

  it('forgives stale failures outside the window', () => {
    const store = memoryStore();
    const now = 1_000_000;
    // (maxFails - 1) failures, then a gap longer than the window, then one more
    for (let i = 0; i < RATE_LIMIT.maxFails - 1; i++) recordFailure(store, 'user:alice', now);
    const later = now + RATE_LIMIT.windowMs + 1;
    recordFailure(store, 'user:alice', later);
    expect(checkRateLimit(store, 'user:alice', later).allowed).toBe(true);
  });
});
