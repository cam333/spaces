import { describe, it, expect } from 'vitest';
import { validateUsername, validateShellUserName } from '@/lib/admin/validate';

describe('validateUsername', () => {
  it('accepts normal usernames', () => {
    expect(validateUsername('alice')).toBeNull();
    expect(validateUsername('a.b_c-1')).toBeNull();
  });
  it('rejects traversal and separators', () => {
    expect(validateUsername('../../tmp/x')).toBeTruthy();
    expect(validateUsername('a/b')).toBeTruthy();
    expect(validateUsername('a..b')).toBeTruthy();
    expect(validateUsername('')).toBeTruthy();
    expect(validateUsername(123 as any)).toBeTruthy();
  });
});

describe('validateShellUserName', () => {
  it('accepts normal OS users', () => {
    expect(validateShellUserName('builder')).toBeNull();
  });
  it('accepts Windows account names with spaces', () => {
    expect(validateShellUserName('Jane Doe')).toBeNull();
    expect(validateShellUserName('Dev User 2')).toBeNull();
  });
  it('rejects privileged account names', () => {
    expect(validateShellUserName('root')).toBeTruthy();
    expect(validateShellUserName('Administrator')).toBeTruthy();
    expect(validateShellUserName('admin')).toBeTruthy();
  });
  it('rejects separators, leading dash, traversal, and shell metacharacters', () => {
    expect(validateShellUserName('a/b')).toBeTruthy();
    expect(validateShellUserName('a\\b')).toBeTruthy();
    expect(validateShellUserName('-x')).toBeTruthy();        // ssh option injection
    expect(validateShellUserName(' x')).toBeTruthy();        // leading space
    expect(validateShellUserName('../etc')).toBeTruthy();
    expect(validateShellUserName("a'b")).toBeTruthy();       // quote (ssh-auth-keys.sh SQL)
    expect(validateShellUserName('a;b')).toBeTruthy();
    expect(validateShellUserName('a$b')).toBeTruthy();
  });
});
