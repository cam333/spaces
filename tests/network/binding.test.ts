import { describe, it, expect } from 'vitest';
import {
  classifyAddress,
  resolveBindAddress,
  BindingRefused,
} from '../../bin/lib/network/binding.js';

describe('address classification', () => {
  it('recognises loopback', () => {
    expect(classifyAddress('127.0.0.1')).toBe('loopback');
    expect(classifyAddress('127.5.5.5')).toBe('loopback');
    expect(classifyAddress('::1')).toBe('loopback');
  });

  it('recognises the overlay range Tailscale uses', () => {
    // 100.64/10 is CGNAT space; a tailnet address always lands in it.
    expect(classifyAddress('100.64.0.1')).toBe('overlay');
    expect(classifyAddress('100.100.100.100')).toBe('overlay');
    expect(classifyAddress('100.127.255.255')).toBe('overlay');
    // Just outside the range on both sides.
    expect(classifyAddress('100.63.255.255')).toBe('public');
    expect(classifyAddress('100.128.0.0')).toBe('public');
  });

  it('recognises private ranges', () => {
    expect(classifyAddress('10.0.0.1')).toBe('private');
    expect(classifyAddress('192.168.1.10')).toBe('private');
    expect(classifyAddress('172.16.0.1')).toBe('private');
    expect(classifyAddress('172.31.255.255')).toBe('private');
    expect(classifyAddress('169.254.1.1')).toBe('private'); // link-local
    expect(classifyAddress('fd00::1')).toBe('private'); // unique local
    expect(classifyAddress('fe80::1')).toBe('private'); // link-local v6
  });

  it('does not mistake a neighbouring range for private', () => {
    // 172.16/12 ends at 172.31; a naive "172.x" check would wave these through.
    expect(classifyAddress('172.15.0.1')).toBe('public');
    expect(classifyAddress('172.32.0.1')).toBe('public');
    expect(classifyAddress('11.0.0.1')).toBe('public');
    expect(classifyAddress('193.168.1.1')).toBe('public');
  });

  it('treats a wildcard as its own category, never as private', () => {
    // The single most dangerous value: it binds every interface including the
    // public one, and it must never be reachable by accident.
    expect(classifyAddress('0.0.0.0')).toBe('wildcard');
    expect(classifyAddress('::')).toBe('wildcard');
  });

  it('classifies anything unrecognised as public', () => {
    // Fail closed: an address this does not understand is treated as the most
    // dangerous thing it could be, not the least.
    expect(classifyAddress('8.8.8.8')).toBe('public');
    expect(classifyAddress('not-an-address')).toBe('public');
    expect(classifyAddress('')).toBe('public');
  });
});

describe('bind address resolution', () => {
  it('accepts loopback, overlay and private without ceremony', () => {
    for (const address of ['127.0.0.1', '100.100.100.100', '192.168.1.5']) {
      expect(resolveBindAddress({ iface: address }).address).toBe(address);
    }
  });

  it('refuses a public address unless it is explicitly allowed', () => {
    expect(() => resolveBindAddress({ iface: '8.8.8.8' })).toThrow(BindingRefused);
    const allowed = resolveBindAddress({ iface: '8.8.8.8', allowPublic: true });
    expect(allowed.address).toBe('8.8.8.8');
    expect(allowed.kind).toBe('public');
  });

  it('refuses the wildcard even when public binding is allowed', () => {
    // "I accept the risk on this address" is a different statement from
    // "bind everything", and the flag must not silently grant the second.
    expect(() => resolveBindAddress({ iface: '0.0.0.0', allowPublic: true })).toThrow(
      BindingRefused
    );
    expect(() => resolveBindAddress({ iface: '::', allowPublic: true })).toThrow(BindingRefused);
  });

  it('refuses when no interface was named', () => {
    // There is no sensible default for "which interface serves a shell".
    expect(() => resolveBindAddress({})).toThrow(BindingRefused);
    expect(() => resolveBindAddress({ iface: '' })).toThrow(BindingRefused);
  });

  it('resolves a named interface to its address', () => {
    const interfaces = {
      tailscale0: [{ family: 'IPv4', address: '100.100.100.100', internal: false }],
      eth0: [{ family: 'IPv4', address: '203.0.113.7', internal: false }],
    };
    expect(resolveBindAddress({ iface: 'tailscale0', interfaces }).address).toBe('100.100.100.100');
    expect(() => resolveBindAddress({ iface: 'eth0', interfaces })).toThrow(BindingRefused);
    expect(resolveBindAddress({ iface: 'eth0', interfaces, allowPublic: true }).address).toBe(
      '203.0.113.7'
    );
  });

  it('reports an unknown interface name rather than guessing', () => {
    expect(() => resolveBindAddress({ iface: 'wg9', interfaces: {} })).toThrow(BindingRefused);
  });

  it('explains itself when it refuses', () => {
    // This message is the whole user experience of the safety rail.
    try {
      resolveBindAddress({ iface: '8.8.8.8' });
      expect.unreachable('should have refused');
    } catch (e: any) {
      expect(e).toBeInstanceOf(BindingRefused);
      expect(e.message).toContain('8.8.8.8');
      expect(e.message).toMatch(/public/i);
    }
  });
});
