/**
 * Decide whether a request path is a public static asset that may bypass auth.
 *
 * SECURITY: API routes must NEVER bypass auth via a static-asset suffix.
 * Next.js catch-all routes can be addressed with a trailing ".js"/".css"/etc.
 * (e.g. /api/proxy/models/<id>/v1/chat/completions/x.js), so a naive
 * `pathname.endsWith('.js')` check let unauthenticated requests reach API
 * handlers (security review C1). Suffix checks therefore apply only to
 * non-/api/ paths.
 */
const ASSET_SUFFIXES = [
  '.png', '.ico', '.svg', '.jpg', '.jpeg', '.gif', '.webp',
  '.css', '.js', '.map', '.woff', '.woff2', '.ttf',
];

export function isPublicAssetPath(pathname: string): boolean {
  if (pathname.startsWith('/api/')) return false;
  if (pathname.startsWith('/_next/')) return true;
  if (pathname.startsWith('/favicon')) return true;
  return ASSET_SUFFIXES.some((s) => pathname.endsWith(s));
}
