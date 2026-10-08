'use strict';
// Shared, dependency-free network-trust helpers used by the terminal server's
// WebSocket auth gate. Kept as plain CJS so it can be require()'d by
// bin/terminal-server.js AND imported directly by vitest.
//
// SECURITY: the previous inline checks used `remoteIp.startsWith('172.')`
// (matches all of 172.0.0.0/8, not just the RFC1918 172.16.0.0/12 Docker range)
// and `remoteIp.includes('192.168.')` (substring match). Both were too broad
// and treated public / unrelated addresses as "local". These helpers test the
// actual ranges.

function normalize(ip) {
  if (!ip) return '';
  // strip IPv4-mapped IPv6 prefix (e.g. ::ffff:127.0.0.1)
  return String(ip).replace(/^::ffff:/i, '');
}

function isLoopback(ip) {
  const a = String(ip || '').toLowerCase();
  if (a === '::1') return true;
  const n = normalize(a);
  return /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(n) || n === '127.0.0.1';
}

function octets(ip) {
  const m = normalize(ip).match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (!m) return null;
  const o = m.slice(1).map(Number);
  return o.every((x) => x >= 0 && x <= 255) ? o : null;
}

function isPrivateLan(ip) {
  const o = octets(ip);
  if (!o) return false;
  const [a, b] = o;
  if (a === 10) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 192 && b === 168) return true;
  return false;
}

module.exports = { isLoopback, isPrivateLan };
