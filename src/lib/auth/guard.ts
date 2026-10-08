import { NextResponse } from 'next/server';
import { IS_MULTIUSER } from '@/lib/flags';

/**
 * Revocation check (review M7). The Edge middleware verifies the session HMAC
 * but cannot reach the DB, so it can't honor a bumped token version. These
 * Node-runtime guards close that gap for sensitive routes: they re-read the
 * session cookie, verify it, and compare its `ver` claim to the DB.
 *
 * FAILS OPEN: returns true (revoked) ONLY when a version mismatch is positively
 * confirmed. Any missing/invalid cookie or error returns false so a bug here
 * degrades to the prior behaviour rather than locking everyone out.
 */
function isSessionRevoked(req: Request): boolean {
  try {
    const cookie = req.headers.get('cookie') || '';
    const m = cookie.match(/(?:^|;\s*)spaces-session=([^;]+)/);
    if (!m) return false;
    const token = decodeURIComponent(m[1]);
    const { verifySessionToken } = require('./session') as typeof import('./session');
    const session = verifySessionToken(token);
    if (!session) return false;
    const { getTokenVersion } = require('../admin/db') as typeof import('../admin/db');
    return session.ver !== getTokenVersion(session.sub);
  } catch {
    return false;
  }
}

/**
 * Require admin authority on a request. The single canonical guard for
 * management endpoints (admin/*, server-config, network/{keys,nodes,proxy,
 * identity-PUT,discovered}).
 *
 * - Multiuser mode: trusts `x-auth-role`. Middleware sets this only after
 *   validating a session cookie, and strips any incoming value before
 *   forwarding — so an attacker cannot spoof it.
 * - Desktop mode: the launcher binds to 127.0.0.1, so any request reaching
 *   this code is by definition local. The single OS user who started the
 *   server is the only caller, and is implicitly the admin.
 *
 * Returns null when the request should proceed, or a Response when denied.
 */
export function requireAdmin(req: Request): Response | null {
  if (!IS_MULTIUSER) return null;
  const role = req.headers.get('x-auth-role');
  if (role !== 'admin') {
    return NextResponse.json({ error: 'Admin required' }, { status: 403 });
  }
  if (isSessionRevoked(req)) {
    return NextResponse.json({ error: 'Session no longer valid' }, { status: 401 });
  }
  return null;
}

/**
 * Require an authenticated user (any role).
 *
 * - Desktop mode: the launcher binds to 127.0.0.1 and the single OS user is
 *   implicitly authenticated, so this is a no-op.
 * - Multiuser mode: requires the middleware-set `x-auth-user`. Middleware
 *   strips any incoming value and only sets it after validating the session
 *   cookie, so its presence proves an authenticated session.
 *
 * Returns null when the request should proceed, or a 401 Response when denied.
 */
export function requireUser(req: Request): Response | null {
  if (!IS_MULTIUSER) return null;
  if (!req.headers.get('x-auth-user')) {
    return NextResponse.json({ error: 'Authentication required' }, { status: 401 });
  }
  if (isSessionRevoked(req)) {
    return NextResponse.json({ error: 'Session no longer valid' }, { status: 401 });
  }
  return null;
}
