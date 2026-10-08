/**
 * Validation for admin-created identities (reviews M6 + L2).
 *
 * - Usernames become a filesystem path component (~/.spaces/users/<username>),
 *   so traversal/separators must be rejected to prevent arbitrary read/write.
 * - Shell users flow into ssh/sudo/path operations and (via the service key)
 *   yield a shell as that OS account, so privileged accounts (root/admin) must
 *   not be mappable, and the name must be a safe token.
 */

const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
// Shell users may be OS accounts (esp. on Windows, e.g. "Jane Doe") that
// contain spaces, so a space is allowed in non-leading positions. Path
// separators, traversal, a leading dash (ssh option injection), and all shell
// metacharacters/quotes remain disallowed — the value flows into ssh argv,
// filesystem paths, and (on Linux) the AuthorizedKeysCommand SQL lookup.
const SHELL_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9 ._-]*$/;
const PRIVILEGED_NAMES = new Set(['root', 'admin', 'administrator', 'wheel', 'sudo']);

export function validateUsername(name: unknown): string | null {
  if (typeof name !== 'string' || name.length === 0) return 'Username is required';
  if (name.length > 64) return 'Username must be 64 characters or fewer';
  if (name.includes('..')) return 'Username may not contain ".."';
  if (!NAME_RE.test(name)) {
    return 'Username may only contain letters, digits, dot, underscore, and hyphen (no leading separator)';
  }
  return null;
}

export function validateShellUserName(name: unknown): string | null {
  if (typeof name !== 'string' || name.length === 0) return 'Shell user is required';
  if (name.length > 64) return 'Shell user must be 64 characters or fewer';
  if (name.includes('..')) return 'Shell user may not contain ".."';
  if (!SHELL_NAME_RE.test(name)) {
    return 'Shell user may only contain letters, digits, spaces, dot, underscore, and hyphen (no leading dash or space)';
  }
  if (PRIVILEGED_NAMES.has(name.toLowerCase())) {
    return `Mapping to the privileged account "${name}" is not allowed`;
  }
  return null;
}
