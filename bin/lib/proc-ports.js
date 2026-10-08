'use strict';

/**
 * Which processes are listening on a TCP port, read straight from /proc.
 *
 * The launcher finds a server to stop with `lsof` or `fuser`, and most slim
 * Linux images have neither. Both lookups then came back empty, so
 * `spaces stop` reported nothing running and the server started next
 * failed to replace the old one, leaving it stranded. The kernel publishes
 * the same answer in /proc: listening sockets with their inodes in
 * /proc/net/tcp{,6}, and which process holds which socket in /proc/<pid>/fd.
 *
 * Only processes this user may inspect are visible, which is every process
 * this user could signal anyway.
 */

const LISTEN = '0A';

/** Inodes of the sockets in a /proc/net/tcp{,6} dump listening on `port`. */
function listeningInodes(text, port) {
  const inodes = [];
  for (const line of String(text).split('\n').slice(1)) {
    const cols = line.trim().split(/\s+/);
    if (cols.length < 10) continue;
    const local = cols[1];
    const hexPort = local.slice(local.lastIndexOf(':') + 1);
    if (parseInt(hexPort, 16) === port && cols[3] === LISTEN) inodes.push(cols[9]);
  }
  return inodes;
}

/** Pids holding a socket listening on `port`. `fs` is injectable for tests. */
function pidsListeningOn(port, fs = require('fs')) {
  const inodes = new Set();
  for (const table of ['/proc/net/tcp', '/proc/net/tcp6']) {
    try {
      for (const inode of listeningInodes(fs.readFileSync(table, 'utf-8'), port)) inodes.add(inode);
    } catch { /* table absent: no IPv6, or no /proc at all */ }
  }
  if (!inodes.size) return [];

  let entries;
  try { entries = fs.readdirSync('/proc'); } catch { return []; }
  const pids = [];
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) continue;
    let fds;
    try { fds = fs.readdirSync(`/proc/${entry}/fd`); } catch { continue; } // not ours
    for (const fd of fds) {
      let target;
      try { target = fs.readlinkSync(`/proc/${entry}/fd/${fd}`); } catch { continue; }
      const m = /^socket:\[(\d+)\]$/.exec(target);
      if (m && inodes.has(m[1])) { pids.push(Number(entry)); break; }
    }
  }
  return pids;
}

/**
 * The uids owning sockets that listen on `port`, from /proc/net/tcp{,6}
 * (column 8). Lets a client check that what answers on the Spaces port is
 * its own account's server before handing it a token: another account can
 * bind the port while Spaces is down, and would otherwise be handed the
 * owner's credentials by the next `spaces tui`. Empty where /proc is
 * unavailable.
 */
function listenerUids(port, fs = require('fs')) {
  const uids = new Set();
  for (const table of ['/proc/net/tcp', '/proc/net/tcp6']) {
    let text;
    try { text = fs.readFileSync(table, 'utf-8'); } catch { continue; }
    for (const line of String(text).split('\n').slice(1)) {
      const cols = line.trim().split(/\s+/);
      if (cols.length < 10) continue;
      const local = cols[1];
      if (parseInt(local.slice(local.lastIndexOf(':') + 1), 16) === port && cols[3] === LISTEN) {
        uids.add(Number(cols[7]));
      }
    }
  }
  return [...uids];
}

module.exports = { listeningInodes, pidsListeningOn, listenerUids };
