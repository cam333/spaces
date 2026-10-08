import { describe, it, expect } from 'vitest';
import { serviceInstalled } from '../../bin/lib/service-registration.js';

const fail = () => { throw new Error('command failed'); };

describe('serviceInstalled', () => {
  it('windows: true only for the Spaces task running the Spaces wrapper', () => {
    const ours = '<Exec><Command>"C:\\Users\\me\\.spaces\\spaces-service.cmd"</Command></Exec>';
    expect(serviceInstalled({ platform: 'win32', run: () => ours })).toBe(true);
    expect(serviceInstalled({ platform: 'win32', run: () => '<Command>other.exe</Command>' })).toBe(false);
    expect(serviceInstalled({ platform: 'win32', run: fail })).toBe(false);
  });

  it('macOS: true when launchd lists the Spaces job', () => {
    expect(serviceInstalled({ platform: 'darwin', run: () => '123\t0\tcom.agentspaces.spaces\n' })).toBe(true);
    expect(serviceInstalled({ platform: 'darwin', run: () => '1\t0\tcom.apple.thing\n' })).toBe(false);
    expect(serviceInstalled({ platform: 'darwin', run: fail })).toBe(false);
  });

  it('linux: true when a user or system unit is installed', () => {
    const only = (wanted: string) => (p: string) => p.replace(/\\/g, '/').endsWith(wanted);
    expect(serviceInstalled({ platform: 'linux', homedir: '/home/me', exists: only('.config/systemd/user/spaces.service') })).toBe(true);
    expect(serviceInstalled({ platform: 'linux', homedir: '/home/me', exists: only('/etc/systemd/system/spaces.service') })).toBe(true);
    expect(serviceInstalled({ platform: 'linux', homedir: '/home/me', exists: () => false })).toBe(false);
  });

  it('anything else: false', () => {
    expect(serviceInstalled({ platform: 'freebsd', run: () => 'x', exists: () => true })).toBe(false);
  });
});
