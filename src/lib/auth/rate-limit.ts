/**
 * Brute-force throttling for authentication endpoints (review H6).
 *
 * The logic is pure and operates over an injectable AttemptStore so it can be
 * unit-tested with an in-memory store and a controlled clock. The default
 * store is backed by the admin SQLite DB so lockouts survive restarts.
 *
 * Callers key by both account (`user:<name>`) and client IP (`ip:<addr>`) and
 * deny if EITHER is locked, so neither a single account nor a single source can
 * be brute-forced.
 */

export const RATE_LIMIT = {
  maxFails: 5,
  windowMs: 15 * 60 * 1000, // failures older than this are forgiven
  lockMs: 15 * 60 * 1000,   // lockout duration once the threshold is hit
};

export interface AttemptRecord {
  failCount: number;
  firstFailMs: number;
  lockedUntilMs: number;
}

export interface AttemptStore {
  get(key: string): AttemptRecord | undefined;
  set(key: string, rec: AttemptRecord): void;
  delete(key: string): void;
}

export interface RateLimitResult {
  allowed: boolean;
  retryAfterMs?: number;
}

export function checkRateLimit(store: AttemptStore, key: string, now: number = Date.now()): RateLimitResult {
  const rec = store.get(key);
  if (rec && rec.lockedUntilMs > now) {
    return { allowed: false, retryAfterMs: rec.lockedUntilMs - now };
  }
  return { allowed: true };
}

export function recordFailure(store: AttemptStore, key: string, now: number = Date.now()): void {
  const rec = store.get(key);
  // Start a fresh window if there is no record or the previous window expired.
  if (!rec || now - rec.firstFailMs > RATE_LIMIT.windowMs) {
    store.set(key, { failCount: 1, firstFailMs: now, lockedUntilMs: 0 });
    return;
  }
  const failCount = rec.failCount + 1;
  const lockedUntilMs = failCount >= RATE_LIMIT.maxFails ? now + RATE_LIMIT.lockMs : rec.lockedUntilMs;
  store.set(key, { failCount, firstFailMs: rec.firstFailMs, lockedUntilMs });
}

export function recordSuccess(store: AttemptStore, key: string): void {
  store.delete(key);
}

/** In-memory store — used by tests and as a fallback when SQLite is unavailable. */
export function memoryStore(): AttemptStore {
  const m = new Map<string, AttemptRecord>();
  return {
    get: (k) => m.get(k),
    set: (k, r) => { m.set(k, r); },
    delete: (k) => { m.delete(k); },
  };
}

let _sqliteStore: AttemptStore | null = null;

/** Default store backed by the admin SQLite DB (lockouts survive restarts). */
export function authStore(): AttemptStore {
  if (_sqliteStore) return _sqliteStore;
  try {
    const { getAdminDb } = require('../admin/db') as typeof import('../admin/db');
    const db = getAdminDb();
    db.exec(`
      CREATE TABLE IF NOT EXISTS auth_attempts (
        key TEXT PRIMARY KEY,
        fail_count INTEGER NOT NULL,
        first_fail_ms INTEGER NOT NULL,
        locked_until_ms INTEGER NOT NULL
      );
    `);
    _sqliteStore = {
      get(key) {
        const row = db.prepare('SELECT fail_count, first_fail_ms, locked_until_ms FROM auth_attempts WHERE key = ?').get(key) as
          | { fail_count: number; first_fail_ms: number; locked_until_ms: number }
          | undefined;
        if (!row) return undefined;
        return { failCount: row.fail_count, firstFailMs: row.first_fail_ms, lockedUntilMs: row.locked_until_ms };
      },
      set(key, rec) {
        db.prepare(
          `INSERT INTO auth_attempts (key, fail_count, first_fail_ms, locked_until_ms)
           VALUES (?, ?, ?, ?)
           ON CONFLICT(key) DO UPDATE SET fail_count = excluded.fail_count,
             first_fail_ms = excluded.first_fail_ms, locked_until_ms = excluded.locked_until_ms`
        ).run(key, rec.failCount, rec.firstFailMs, rec.lockedUntilMs);
      },
      delete(key) {
        db.prepare('DELETE FROM auth_attempts WHERE key = ?').run(key);
      },
    };
  } catch {
    // If the DB can't be opened, fall back to in-memory rather than failing open.
    _sqliteStore = memoryStore();
  }
  return _sqliteStore;
}

/**
 * Best-effort client IP for per-source throttling. Trusts X-Forwarded-For only
 * when an upstream proxy is declared via SPACES_TRUSTED_PROXY (review L8);
 * otherwise returns 'unknown' so all unproxied callers share one bucket rather
 * than letting an attacker rotate a spoofed header to dodge the limit.
 */
export function clientIp(request: Request): string {
  if (process.env.SPACES_TRUSTED_PROXY === '1') {
    const xff = request.headers.get('x-forwarded-for');
    if (xff) return xff.split(',')[0].trim();
  }
  return 'unknown';
}
