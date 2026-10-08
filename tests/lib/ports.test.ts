import { describe, it, expect } from 'vitest';
import { resolveAccountPort } from '../../bin/lib/ports.js';

// Two people using --remote against one host each run their own server.

const none = () => false;

describe('resolveAccountPort', () => {
  it('uses a port the user chose, whatever holds the default', () => {
    expect(resolveAccountPort({ chosen: 4000, isMine: () => false, isListening: none, save: () => { throw new Error('no'); } }))
      .toEqual({ port: 4000, claimed: false });
  });

  it('takes the default when it is free or already this account\'s', () => {
    expect(resolveAccountPort({ chosen: undefined, isMine: () => true, isListening: none, save: () => {} }))
      .toEqual({ port: 3457, claimed: false });
  });

  it('claims and saves the next free pair when another account holds the default', () => {
    const saved: number[] = [];
    const busy = new Set([3458, 3402]); // 3458 itself; 3459's internal port (3402)
    const r = resolveAccountPort({
      chosen: undefined,
      isMine: (p) => p !== 3457,
      isListening: (p) => busy.has(p),
      save: (p) => saved.push(p),
    });
    expect(r).toEqual({ port: 3460, claimed: true });
    expect(saved).toEqual([3460]);
  });

  it('still returns the port when it cannot be saved', () => {
    const r = resolveAccountPort({ chosen: undefined, isMine: () => false, isListening: none, save: () => { throw new Error('read-only'); } });
    expect(r).toEqual({ port: 3458, claimed: true });
  });

  it('falls back to the default when nothing nearby is free', () => {
    const r = resolveAccountPort({ chosen: undefined, isMine: () => false, isListening: () => true, save: () => {} });
    expect(r).toEqual({ port: 3457, claimed: false });
  });
});
