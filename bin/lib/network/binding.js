/**
 * Which interface the federation listener is allowed to bind.
 *
 * This is the primary security control in the whole feature. Everything else —
 * the handshake, the capability restrictions, the audit log — is defense in
 * depth beneath the question "who can open a TCP connection to this at all".
 * On an overlay network the answer is "devices you personally enrolled", which
 * is a stronger guarantee than any code here could provide.
 *
 * Two rules follow from that, and both are enforced rather than advised:
 *
 *   1. There is no default. "Which interface serves a shell" has no sensible
 *      answer that can be guessed on the user's behalf.
 *   2. Unrecognised means public. An address this module does not understand
 *      is treated as the most dangerous thing it might be.
 *
 * The wildcard is refused unconditionally, including under `allowPublic`.
 * "I accept the risk on this address" and "bind every interface" are different
 * statements, and the flag grants only the first.
 */

const os = require('os');

/** Thrown instead of binding something that was not clearly authorised. */
class BindingRefused extends Error {
  constructor(message) {
    super(message);
    this.name = 'BindingRefused';
  }
}

function ipv4Octets(address) {
  const parts = address.split('.');
  if (parts.length !== 4) return null;
  const octets = parts.map((p) => (/^\d{1,3}$/.test(p) ? Number(p) : NaN));
  return octets.some((o) => Number.isNaN(o) || o > 255) ? null : octets;
}

/**
 * `loopback` | `overlay` | `private` | `wildcard` | `public`.
 *
 * `overlay` is 100.64/10 — CGNAT space, where a Tailscale address always
 * lands. It is called out separately from `private` because it is the
 * recommended configuration and worth naming in logs and errors.
 */
function classifyAddress(address) {
  if (!address) return 'public';
  const addr = String(address).trim().toLowerCase();

  if (addr === '0.0.0.0' || addr === '::' || addr === '[::]') return 'wildcard';
  if (addr === '::1' || addr === '[::1]') return 'loopback';

  // IPv6 unique-local (fc00::/7) and link-local (fe80::/10).
  if (/^f[cd][0-9a-f]{2}:/.test(addr)) return 'private';
  if (/^fe[89ab][0-9a-f]:/.test(addr)) return 'private';

  const octets = ipv4Octets(addr);
  if (!octets) return 'public';
  const [a, b] = octets;

  if (a === 127) return 'loopback';
  // 100.64.0.0 – 100.127.255.255
  if (a === 100 && b >= 64 && b <= 127) return 'overlay';
  if (a === 10) return 'private';
  // 172.16.0.0 – 172.31.255.255 — note the upper bound; "172.x" is wrong.
  if (a === 172 && b >= 16 && b <= 31) return 'private';
  if (a === 192 && b === 168) return 'private';
  if (a === 169 && b === 254) return 'private';

  return 'public';
}

/** Every non-internal address of a named interface, IPv4 first. */
function addressesOf(name, interfaces) {
  const entries = interfaces[name];
  if (!entries) return [];
  return entries
    .filter((e) => !e.internal)
    .sort((x, y) => (x.family === 'IPv4' ? -1 : 1) - (y.family === 'IPv4' ? -1 : 1))
    .map((e) => e.address);
}

/**
 * Resolves `iface` — an interface name or a literal address — to something
 * safe to bind, or throws [`BindingRefused`] explaining why not.
 *
 * The error message is the entire user experience of this rail, so it names
 * the address, the category it fell into, and the flag that would permit it.
 */
function resolveBindAddress({ iface, allowPublic = false, interfaces = os.networkInterfaces() } = {}) {
  if (!iface) {
    throw new BindingRefused(
      'federation needs an interface to bind: pass --iface <name|address> ' +
        '(there is no default, because the answer decides who can reach a shell)'
    );
  }

  const named = addressesOf(iface, interfaces);
  const candidates = named.length > 0 ? named : [iface];
  if (named.length === 0 && Object.prototype.hasOwnProperty.call(interfaces, iface)) {
    throw new BindingRefused(`interface "${iface}" has no usable address`);
  }
  // A name that matches no interface and is not an address is a typo, and a
  // typo must not fall through to being treated as a hostname to resolve.
  if (named.length === 0 && !ipv4Octets(iface) && !iface.includes(':') && iface !== '0.0.0.0') {
    throw new BindingRefused(
      `no interface named "${iface}" on this machine ` +
        `(available: ${Object.keys(interfaces).join(', ') || 'none'})`
    );
  }

  const address = candidates[0];
  const kind = classifyAddress(address);

  if (kind === 'wildcard') {
    throw new BindingRefused(
      `refusing to bind ${address}: that is every interface at once, including any public one. ` +
        'Name the single interface federation should serve.'
    );
  }
  if (kind === 'public' && !allowPublic) {
    throw new BindingRefused(
      `refusing to bind ${address}: it is a public address, and federation serves a channel ` +
        'that can start processes. Prefer an overlay network (Tailscale, WireGuard). ' +
        'If this machine genuinely cannot join one, pass --federation-public-interface to accept the risk.'
    );
  }

  return { address, kind, iface };
}

module.exports = { classifyAddress, resolveBindAddress, BindingRefused };
