/**
 * The middleware's half of desktop-mode authentication — see
 * bin/lib/desktop-auth.js for the scheme and why it exists. The middleware
 * runs where Node's crypto module is unavailable, so this re-implements the
 * checks it needs with WebCrypto. tests/security/desktop-auth-edge.test.ts
 * verifies tokens signed by the Node side, so the two cannot drift apart.
 */

export const DESKTOP_COOKIE = 'spaces-desktop';
const MIN_SECRET_CHARS = 32;
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

/** A SPACES_ALLOWED_ORIGINS entry as a bare hostname; see the Node side. */
function allowedName(entry: string): string {
  const e = entry.trim().toLowerCase();
  if (!e) return '';
  if (e.includes('://')) {
    try { return new URL(e).hostname; } catch { return ''; }
  }
  return e.replace(/:\d+$/, '');
}

/** Same rule as the Node side's isLocalHost. */
export function isLocalHost(host: string | null | undefined, allowed: string[] = []): boolean {
  if (typeof host !== 'string' || !host) return false;
  const m = /^(\[[0-9a-fA-F:.]+\]|[^:[\]]+)(?::(\d{1,5}))?$/.exec(host.trim());
  if (!m) return false;
  const name = m[1].toLowerCase();
  if (LOOPBACK.has(name)) return true;
  return allowed.some((a) => allowedName(a) === name);
}

/** Same rule as the Node side's originIsSelf. */
export function originIsSelf(origin: string | null, host: string | null): boolean {
  if (!origin || !host) return false;
  const o = origin.trim().toLowerCase();
  const h = host.trim().toLowerCase();
  return o === `http://${h}` || o === `https://${h}`;
}

/**
 * Whether a request that authenticates with the cookie alone came from
 * Spaces' own pages. Every port on localhost is the same *site*, so the
 * SameSite=Strict cookie also rides along on requests a page from another
 * local port makes; the browser's Sec-Fetch-Site (or, failing that, the
 * Origin) says whether this one is really ours. `none` is the user typing
 * the address or following a bookmark.
 */
export function cameFromSelf(headers: Headers): boolean {
  const site = headers.get('sec-fetch-site');
  if (site) return site === 'same-origin' || site === 'none';
  const origin = headers.get('origin');
  return !origin || originIsSelf(origin, headers.get('host'));
}

/** Constant-time string equality; never true for empty input. */
export function timingSafeEqualStr(a: string, b: string): boolean {
  if (!a || !b) return false;
  const aBytes = new TextEncoder().encode(a);
  const bBytes = new TextEncoder().encode(b);
  if (aBytes.length !== bBytes.length) return false;
  let diff = 0;
  for (let i = 0; i < aBytes.length; i++) diff |= aBytes[i] ^ bBytes[i];
  return diff === 0;
}

function base64url(bytes: ArrayBuffer): string {
  let s = '';
  for (const b of new Uint8Array(bytes)) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function fromBase64url(s: string): string {
  const padded = s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4);
  const bin = atob(padded);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

export function usableSecret(secret: string | undefined): secret is string {
  return typeof secret === 'string' && secret.length >= MIN_SECRET_CHARS;
}

/** True for a valid, unexpired token of one of `kinds`. */
export async function verifyDesktopToken(
  secret: string,
  token: string | undefined,
  kinds: string[],
): Promise<boolean> {
  if (!usableSecret(secret) || !token) return false;
  const parts = token.split('.');
  if (parts.length !== 2 || !parts[0] || !parts[1]) return false;
  const key = await crypto.subtle.importKey(
    'raw', new TextEncoder().encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign'],
  );
  const sig = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(`spaces-desktop:${parts[0]}`));
  if (!timingSafeEqualStr(base64url(sig), parts[1])) return false;
  try {
    const payload = JSON.parse(fromBase64url(parts[0]));
    return kinds.includes(payload?.k)
      && typeof payload.exp === 'number'
      && payload.exp >= Math.floor(Date.now() / 1000);
  } catch {
    return false;
  }
}

/** True for a valid, unexpired browser session token. */
export function verifyDesktopSession(secret: string, token: string | undefined): Promise<boolean> {
  return verifyDesktopToken(secret, token, ['session']);
}

/** True when a header credential proves the owner — see isOwnerCredential. */
export async function isOwnerCredential(secret: string, presented: string): Promise<boolean> {
  if (!usableSecret(secret) || !presented) return false;
  if (timingSafeEqualStr(presented, secret)) return true;
  return verifyDesktopToken(secret, presented, ['session', 'api']);
}

/** The credential presented as a bearer or in x-spaces-internal, or ''. */
export function presentedSecret(headers: Headers): string {
  const auth = headers.get('authorization');
  if (auth) {
    const m = /^Bearer\s+(\S+)\s*$/i.exec(auth);
    if (m) return m[1];
  }
  return (headers.get('x-spaces-internal') || '').trim();
}
