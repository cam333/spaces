'use strict';
// Per-user SSH key helpers (review M1). Replaces the single shared service key
// with one keypair per shell user, so compromising one user's key does not
// grant access as every user. Plain CJS so terminal-server.js can require it
// and vitest can import it.
const path = require('path');

// Strict OS-username allowlist. Also used as the on-disk key filename, so it
// must reject path separators, traversal, and a leading dash (which ssh would
// parse as an option — review L3).
const VALID_SHELL_USER = /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,31}$/;

function isValidShellUser(name) {
  if (typeof name !== 'string') return false;
  if (name === '.' || name === '..') return false;
  return VALID_SHELL_USER.test(name);
}

function userKeyPath(spacesDir, shellUser) {
  if (!isValidShellUser(shellUser)) return null;
  return path.join(spacesDir, 'keys', shellUser);
}

function userKeyPubPath(spacesDir, shellUser) {
  const p = userKeyPath(spacesDir, shellUser);
  return p ? p + '.pub' : null;
}

module.exports = { isValidShellUser, userKeyPath, userKeyPubPath };
