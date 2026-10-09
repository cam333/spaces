import { describe, it, expect } from 'vitest';
import { ask, readLine } from '../../bin/lib/prompt.js';

/** A `read` that serves `text` one byte at a time, then end of input. */
function typed(text: string) {
  const bytes = Buffer.from(text, 'utf-8');
  let i = 0;
  return (buf: Buffer) => {
    if (i >= bytes.length) return 0;
    buf[0] = bytes[i++];
    return 1;
  };
}

describe('readLine', () => {
  it('reads up to the newline, without a carriage return', () => {
    expect(readLine({ read: typed('yes\r\nmore') })).toBe('yes');
  });

  it('returns what there is at end of input, or null for nothing', () => {
    expect(readLine({ read: typed('partial') })).toBe('partial');
    expect(readLine({ read: typed('') })).toBeNull();
  });

  it('keeps multi-byte characters whole', () => {
    expect(readLine({ read: typed('ü\n') })).toBe('ü');
  });
});

describe('ask', () => {
  const quiet = () => {};

  it('takes the fallback for an empty answer', () => {
    expect(ask('? ', ['y', 'n'], 'y', { write: quiet, read: typed('\n') })).toBe('y');
  });

  it('takes a letter or its word, in any case', () => {
    expect(ask('? ', ['y', 'n'], 'y', { write: quiet, read: typed('N\n') })).toBe('n');
    expect(ask('? ', ['u', 'c', 'q'], null, { write: quiet, read: typed('Update\n') })).toBe('u');
  });

  it('asks again until the answer means something, with no fallback', () => {
    const asked: string[] = [];
    const answer = ask('pick: ', ['u', 'c', 'q'], null, { write: (s: string) => asked.push(s), read: typed('\nyikes\nc\n') });
    expect(answer).toBe('c');
    expect(asked.filter((s) => s === 'pick: ')).toHaveLength(3);
  });

  it('gives the fallback, or null, at end of input', () => {
    expect(ask('? ', ['y', 'n'], 'y', { write: quiet, read: typed('') })).toBe('y');
    expect(ask('? ', ['u', 'c', 'q'], null, { write: quiet, read: typed('') })).toBeNull();
  });
});
