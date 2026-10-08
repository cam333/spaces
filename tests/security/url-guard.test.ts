import { describe, it, expect } from 'vitest';
import { rejectPrivateUrl, sanitizeProxyPath } from '@/lib/network/url-guard';

describe('rejectPrivateUrl', () => {
  it('blocks loopback, private, link-local and metadata targets', () => {
    expect(rejectPrivateUrl('http://127.0.0.1')).toBeTruthy();
    expect(rejectPrivateUrl('http://localhost:8080')).toBeTruthy();
    expect(rejectPrivateUrl('http://10.0.0.5')).toBeTruthy();
    expect(rejectPrivateUrl('http://192.168.1.1')).toBeTruthy();
    expect(rejectPrivateUrl('http://172.16.0.1')).toBeTruthy();
    expect(rejectPrivateUrl('http://169.254.169.254/latest/meta-data/')).toBeTruthy();
    expect(rejectPrivateUrl('http://0.0.0.0')).toBeTruthy();
    expect(rejectPrivateUrl('http://[::1]')).toBeTruthy();
  });

  it('rejects non-http(s) and unparseable URLs', () => {
    expect(rejectPrivateUrl('file:///etc/passwd')).toBeTruthy();
    expect(rejectPrivateUrl('not a url')).toBeTruthy();
  });

  it('allows public hosts', () => {
    expect(rejectPrivateUrl('https://node.example.com')).toBeNull();
    expect(rejectPrivateUrl('http://203.0.113.5:3457')).toBeNull();
  });
});

describe('sanitizeProxyPath', () => {
  it('accepts normal federation path segments', () => {
    expect(sanitizeProxyPath(['sessions', '123'])).toBe('sessions/123');
    expect(sanitizeProxyPath(['workspaces'])).toBe('workspaces');
  });

  it('rejects traversal and separator tricks', () => {
    expect(sanitizeProxyPath(['..', 'etc'])).toBeNull();
    expect(sanitizeProxyPath(['a', '..', 'b'])).toBeNull();
    expect(sanitizeProxyPath(['a/b'])).toBeNull();
    expect(sanitizeProxyPath(['a\\b'])).toBeNull();
    expect(sanitizeProxyPath(['a%2fb'])).toBeNull();
    expect(sanitizeProxyPath(['.'])).toBeNull();
    expect(sanitizeProxyPath([''])).toBeNull();
  });
});
