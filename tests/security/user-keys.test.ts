import { describe, it, expect } from 'vitest';
import userKeys from '../../bin/lib/user-keys.js';
const { isValidShellUser, userKeyPath, userKeyPubPath } = userKeys;

describe('isValidShellUser', () => {
  it('accepts normal OS usernames', () => {
    expect(isValidShellUser('alice')).toBe(true);
    expect(isValidShellUser('build_bot')).toBe(true);
    expect(isValidShellUser('svc-01')).toBe(true);
  });
  it('rejects traversal, separators, leading dash, and junk', () => {
    expect(isValidShellUser('../etc')).toBe(false);
    expect(isValidShellUser('a/b')).toBe(false);
    expect(isValidShellUser('a\\b')).toBe(false);
    expect(isValidShellUser('-oProxyCommand=x')).toBe(false);
    expect(isValidShellUser('.')).toBe(false);
    expect(isValidShellUser('..')).toBe(false);
    expect(isValidShellUser('')).toBe(false);
    expect(isValidShellUser('has space')).toBe(false);
  });
});

describe('userKeyPath', () => {
  it('builds <spacesDir>/keys/<user> for valid users', () => {
    const p = userKeyPath('/home/svc/.spaces', 'alice');
    expect(p).toMatch(/[\\/]keys[\\/]alice$/);
    expect(userKeyPubPath('/home/svc/.spaces', 'alice')).toMatch(/[\\/]keys[\\/]alice\.pub$/);
  });
  it('returns null for invalid users (no traversal into the path)', () => {
    expect(userKeyPath('/home/svc/.spaces', '../../etc/x')).toBeNull();
    expect(userKeyPath('/home/svc/.spaces', 'a/b')).toBeNull();
  });
});
