import { describe, it, expect } from 'vitest';
// net-trust.js is plain CJS so it can be required by both terminal-server.js and tests.
import netTrust from '../../bin/lib/net-trust.js';
const { isLoopback, isPrivateLan } = netTrust;

describe('isLoopback', () => {
  it('matches only true loopback', () => {
    expect(isLoopback('127.0.0.1')).toBe(true);
    expect(isLoopback('::1')).toBe(true);
    expect(isLoopback('::ffff:127.0.0.1')).toBe(true);
    expect(isLoopback('192.168.1.5')).toBe(false);
    expect(isLoopback('172.20.0.3')).toBe(false);
    expect(isLoopback('10.0.0.1')).toBe(false);
    expect(isLoopback('')).toBe(false);
  });
});

describe('isPrivateLan', () => {
  it('matches true RFC1918 ranges only', () => {
    expect(isPrivateLan('172.16.0.1')).toBe(true);
    expect(isPrivateLan('172.31.255.255')).toBe(true);
    expect(isPrivateLan('192.168.0.1')).toBe(true);
    expect(isPrivateLan('10.1.2.3')).toBe(true);
  });
  it('rejects 172.x outside 16-31 (the old startsWith("172.") bug)', () => {
    expect(isPrivateLan('172.0.0.1')).toBe(false);
    expect(isPrivateLan('172.15.0.1')).toBe(false);
    expect(isPrivateLan('172.32.0.1')).toBe(false);
    expect(isPrivateLan('172.217.0.1')).toBe(false); // public (Google)
  });
  it('rejects substring-style false positives (the old includes("192.168.") bug)', () => {
    expect(isPrivateLan('8.192.168.1')).toBe(false);
    expect(isPrivateLan('192.168.x.y')).toBe(false);
  });
});
