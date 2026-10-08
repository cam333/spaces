import { AsyncLocalStorage } from 'async_hooks';
import { NextRequest } from 'next/server';
import os from 'os';
import { IS_MULTIUSER } from '@/lib/flags';

const userStore = new AsyncLocalStorage<string>();

/**
 * Extract the authenticated username from SSO headers.
 * Falls back to the OS user for local development.
 */
export function getAuthUser(request: NextRequest): string {
  // Only trust x-auth-user when auth middleware is active (multiuser mode).
  // The middleware validates the session and sets this header.
  // Without auth middleware, a client could spoof this header.
  if (IS_MULTIUSER) {
    return request.headers.get('x-auth-user') || os.userInfo().username;
  }
  return os.userInfo().username;
}

/**
 * Like getAuthUser but FAILS CLOSED in multiuser mode: returns null when the
 * middleware-set x-auth-user header is absent, instead of falling back to the
 * server's OS account. Use this on endpoints that can be reached without the
 * standard middleware gate (e.g. previously the /api/auth/totp/* routes) so an
 * unauthenticated caller cannot act as the OS user (review H2). Desktop mode
 * has a single local user, so the OS user is the correct identity there.
 */
export function getAuthUserOrNull(request: Request): string | null {
  if (IS_MULTIUSER) {
    return request.headers.get('x-auth-user');
  }
  return os.userInfo().username;
}

/**
 * Run a function within a user context, so getDb() and other
 * user-scoped operations resolve to the correct user.
 */
export function withUser<T>(username: string, fn: () => T): T {
  return userStore.run(username, fn);
}

/**
 * Extract the user's role from request headers.
 * Desktop mode defaults to 'admin'; server mode reads the middleware-set header.
 */
export function getAuthRole(request: NextRequest): string {
  if (!IS_MULTIUSER) return 'admin';
  return request.headers.get('x-auth-role') || 'user';
}

/**
 * Get the current user from AsyncLocalStorage context.
 * Falls back to the OS user for local dev or background tasks.
 */
export function getCurrentUser(): string {
  const user = userStore.getStore();
  if (!user) return os.userInfo().username;
  return user;
}
