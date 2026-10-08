/**
 * SSRF guards for the federation layer.
 *
 * Federation requests fetch a node's URL with the node's stored bearer token
 * attached, so an attacker-controlled URL becomes a credentialed SSRF primitive
 * (cloud metadata, internal services). These guards are applied at every point
 * a node URL is accepted (add AND update) and again at proxy egress.
 */

/**
 * Returns an error string if the URL points at a private/loopback/link-local/
 * metadata target or is otherwise unsafe to fetch, or null if it is allowed.
 */
export function rejectPrivateUrl(rawUrl: string): string | null {
  let parsed: URL;
  try { parsed = new URL(rawUrl); } catch { return 'URL is not parseable'; }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    return 'URL must be http or https';
  }
  const host = parsed.hostname.toLowerCase();
  const blockedHosts = ['localhost', '0.0.0.0'];
  if (blockedHosts.includes(host)) return 'Loopback hostnames are not allowed';
  const v4 = host.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/);
  if (v4) {
    const [a, b] = [parseInt(v4[1], 10), parseInt(v4[2], 10)];
    if (a === 127) return 'Loopback IPs are not allowed';
    if (a === 10) return 'Private IPs are not allowed';
    if (a === 192 && b === 168) return 'Private IPs are not allowed';
    if (a === 172 && b >= 16 && b <= 31) return 'Private IPs are not allowed';
    if (a === 169 && b === 254) return 'Link-local IPs are not allowed';
    if (a === 0) return 'Reserved IPs are not allowed';
  }
  if (host.startsWith('[::1') || host === '::1') return 'Loopback IPs are not allowed';
  if (host.startsWith('[fe80') || host.startsWith('fe80:')) return 'Link-local IPs are not allowed';
  if (host.startsWith('[fc') || host.startsWith('[fd') || host.startsWith('fc') || host.startsWith('fd')) {
    return 'Private IPs are not allowed';
  }
  return null;
}

/**
 * Validate and join proxy path segments. Returns the joined path, or null if
 * any segment is empty, a traversal token, or contains a path separator
 * (literal or percent-encoded) — preventing a caller from escaping the
 * /api/network/ prefix the proxy is meant to forward to.
 */
export function sanitizeProxyPath(segments: string[]): string | null {
  if (!Array.isArray(segments) || segments.length === 0) return null;
  for (const seg of segments) {
    if (!seg || seg === '.' || seg === '..') return null;
    if (/[\/\\]/.test(seg)) return null;
    if (/%2f|%5c/i.test(seg)) return null;
  }
  return segments.join('/');
}
