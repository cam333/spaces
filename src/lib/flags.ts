/**
 * Runtime flags for the consolidated Spaces app.
 *
 * IS_MULTIUSER — when true, middleware enforces login, admin routes are live,
 * and config.ts resolves shell users via the admin DB. When false (default),
 * the app runs as a solo desktop workspace manager with no login.
 *
 * Federation is always on. The /api/network/* routes are live regardless of
 * mode; list endpoints return empty arrays when no peer nodes are configured.
 */
export const IS_MULTIUSER: boolean =
  process.env.SPACES_MULTIUSER === '1' ||
  process.env.NEXT_PUBLIC_MULTIUSER === '1';

export const IS_DESKTOP: boolean = !IS_MULTIUSER;

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
    hasNetwork: true,
    hasMultiuser: IS_MULTIUSER,
    isDesktop: IS_DESKTOP,
    basePath: process.env.SPACES_BASE_PATH || '',
  };
}
