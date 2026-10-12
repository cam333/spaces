'use strict';

/**
 * The flags `spaces` recognises, so a typo is reported instead of being
 * ignored. An ignored flag used to fall through to starting a server in the
 * foreground — which first stops whatever server is already running on the
 * port — so `spaces --verison` was anything but harmless.
 */

/** Flags followed by a value. */
const VALUE_FLAGS = new Set([
  '--port',
  '--federate',
  '--federation-port',
  '--base-path',
  '--remote',
  // The terminal client's own (spaces tui / spaces --remote).
  '--url',
  '--token',
]);

/** Flags that stand alone. */
const SWITCHES = new Set([
  '--setup',
  '--multiuser',
  '--no-multiuser',
  '--desktop',
  '--experimental',
  '--federation-public-interface',
  '--help', '-h',
  '--version', '-v',
  // spaces start (what --remote runs on the other machine).
  '--print-token',
  // spaces start, for spaces open --remote.
  '--print-login',
  // spaces --remote: install or update the other machine without asking.
  '--yes', '-y',
  // spaces status, for --remote to read.
  '--porcelain',
]);

/**
 * Commands whose arguments belong to another script, which validates them
 * itself (`spaces service <action>`, `spaces reset-totp <user>`, ...).
 */
const PASSTHROUGH_COMMANDS = new Set(['service', 'reset-totp', 'uninstall', 'upgrade']);

/** The first argument that looks like a flag but is not one, or null. */
function findUnknownFlag(args) {
  if (args.length && PASSTHROUGH_COMMANDS.has(args[0])) return null;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (VALUE_FLAGS.has(a)) { i++; continue; }
    if (SWITCHES.has(a)) continue;
    if (a.length > 1 && a.startsWith('-')) return a;
  }
  return null;
}

module.exports = { findUnknownFlag, VALUE_FLAGS, SWITCHES, PASSTHROUGH_COMMANDS };
