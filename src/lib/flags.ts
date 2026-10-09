/**
 * Runtime flags for the consolidated Spaces app.
 *
 * IS_MULTIUSER — when true, middleware enforces login, admin routes are live,
 * and config.ts resolves shell users via the admin DB. When false (default),
 * the app runs as a solo desktop workspace manager with no login.
 *
 * EXPERIMENTAL — `--experimental` or SPACES_EXPERIMENTAL=1. Federation is
 * part of it: without it the launcher answers /api/network/* with a 404, so
 * the web app must not offer the Network page either.
 */
export const IS_MULTIUSER: boolean =
  process.env.SPACES_MULTIUSER === '1' ||
  process.env.NEXT_PUBLIC_MULTIUSER === '1';

export const IS_DESKTOP: boolean = !IS_MULTIUSER;

export const IS_EXPERIMENTAL: boolean = process.env.SPACES_EXPERIMENTAL === '1';

/** Returned from /api/tier (name kept for route compatibility). */
export function getTierFlags() {
  let version = '0.0.0';
  try { version = require('../../package.json').version; } catch {}
  return {
    tier: IS_MULTIUSER ? 'team' : 'community',
    version,
    hasAuth: IS_MULTIUSER,
    hasAdmin: IS_MULTIUSER,
    hasCollaboration: IS_MULTIUSER,
    hasNetwork: IS_EXPERIMENTAL,
    hasMultiuser: IS_MULTIUSER,
    isDesktop: IS_DESKTOP,
    basePath: process.env.SPACES_BASE_PATH || '',
  };
}
