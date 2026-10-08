import { describe, it, expect } from 'vitest';
import { parsePasswdUsers, parseSimpleUserList } from '@/lib/admin/os-users';

describe('parsePasswdUsers (Linux /etc/passwd)', () => {
  const passwd = [
    'root:x:0:0:root:/root:/bin/bash',
    'daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin',
    'nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin',
    'svc-build:x:997:997::/home/svc-build:/bin/false',     // system-ish, nologin-ish shell
    'alice:x:1000:1000:Alice:/home/alice:/bin/bash',
    'bob:x:1001:1001:Bob:/home/bob:/usr/bin/zsh',
    'systemd-network:x:998:998:systemd:/run/systemd:/usr/sbin/nologin',
  ].join('\n');

  it('returns only real login users (uid>=1000, real shell, no system accounts)', () => {
    const users = parsePasswdUsers(passwd);
    expect(users).toContain('alice');
    expect(users).toContain('bob');
    expect(users).not.toContain('root');
    expect(users).not.toContain('daemon');
    expect(users).not.toContain('nobody');
    expect(users).not.toContain('svc-build');       // /bin/false
    expect(users).not.toContain('systemd-network');  // uid < 1000
  });
});

describe('parseSimpleUserList (Windows Get-LocalUser names / macOS dscl)', () => {
  it('filters system/built-in/privileged and underscore accounts', () => {
    const out = ['Jane Doe', 'devuser', 'Administrator', 'Guest', 'DefaultAccount', 'WDAGUtilityAccount', '_spotlight', ''].join('\r\n');
    const users = parseSimpleUserList(out);
    expect(users).toContain('Jane Doe');
    expect(users).toContain('devuser');
    expect(users).not.toContain('Administrator');
    expect(users).not.toContain('Guest');
    expect(users).not.toContain('DefaultAccount');
    expect(users).not.toContain('WDAGUtilityAccount');
    expect(users).not.toContain('_spotlight');
    expect(users).not.toContain('');
  });
});
