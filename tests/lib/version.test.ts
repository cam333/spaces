import { describe, it, expect } from 'vitest';
import { isNewer } from '../../bin/lib/version.js';

describe('isNewer', () => {
  it('compares numbers, not strings', () => {
    expect(isNewer('0.10.0', '0.9.10')).toBe(true);
    expect(isNewer('0.9.10', '0.10.0')).toBe(false);
    expect(isNewer('1.0.0', '0.10.0-beta.12')).toBe(true);
    expect(isNewer('1.0.1', '1.0.0')).toBe(true);
    expect(isNewer('1.0.0', '1.0.0')).toBe(false);
  });

  it('ranks a release above its pre-releases, and pre-releases in order', () => {
    expect(isNewer('1.0.0', '1.0.0-beta.2')).toBe(true);
    expect(isNewer('1.0.0-beta.2', '1.0.0')).toBe(false);
    expect(isNewer('1.0.0-beta.10', '1.0.0-beta.9')).toBe(true);
  });

  it('is false for anything missing', () => {
    expect(isNewer('', '1.0.0')).toBe(false);
    expect(isNewer('1.0.0', undefined as any)).toBe(false);
  });
});
