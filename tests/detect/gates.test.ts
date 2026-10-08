import { describe, it, expect } from 'vitest';
import { matchGate, compileGate } from '../../bin/lib/runtime/detect/gates.js';

const m = (gate: any, text: string) => matchGate(compileGate(gate), text);

describe('gates', () => {
  it('contains is case-insensitive and requires every string', () => {
    expect(m({ contains: ['Do you want'] }, 'do you want to proceed?')).toBe(true);
    expect(m({ contains: ['do you want', 'proceed'] }, 'do you want to proceed?')).toBe(true);
    expect(m({ contains: ['do you want', 'absent'] }, 'do you want to proceed?')).toBe(false);
  });

  it('regex matches against the whole text including newlines', () => {
    expect(m({ regex: ['proceed\\?$'] }, 'do you want to proceed?')).toBe(true);
    expect(m({ regex: ['^one[\\s\\S]*three$'] }, 'one\ntwo\nthree')).toBe(true);
    expect(m({ regex: ['^nope'] }, 'one\ntwo')).toBe(false);
  });

  it('lineRegex matches when any single line matches', () => {
    expect(m({ lineRegex: ['^\\s*❯\\s*1\\.\\s*yes'] }, 'header\n  ❯ 1. Yes, proceed\nfooter')).toBe(false);
    expect(m({ lineRegex: ['^\\s*> 1\\. yes'] }, 'x\n> 1. yes\ny')).toBe(true);
  });

  it('lineRegex throws on invalid pattern like (?i)', () => {
    expect(() => compileGate({ lineRegex: ['(?i)^\\s*❯\\s*1\\.\\s*yes'] })).toThrow();
  });

  it('lineRegex throws on unclosed bracket pattern', () => {
    expect(() => compileGate({ lineRegex: ['([unclosed'] })).toThrow();
  });

  it('any passes when at least one sub-gate passes', () => {
    const gate = { any: [{ contains: ['alpha'] }, { contains: ['beta'] }] };
    expect(m(gate, 'has beta here')).toBe(true);
    expect(m(gate, 'has neither')).toBe(false);
  });

  it('all requires every sub-gate', () => {
    const gate = { all: [{ contains: ['alpha'] }, { contains: ['beta'] }] };
    expect(m(gate, 'alpha and beta')).toBe(true);
    expect(m(gate, 'alpha only')).toBe(false);
  });

  it('not passes only when no sub-gate passes', () => {
    const gate = { not: [{ contains: ['esc to cancel'] }] };
    expect(m(gate, 'ready')).toBe(true);
    expect(m(gate, 'esc to cancel')).toBe(false);
  });

  it('multiple keys on one gate are ANDed together', () => {
    const gate = { contains: ['proceed'], not: [{ contains: ['cancelled'] }] };
    expect(m(gate, 'proceed now')).toBe(true);
    expect(m(gate, 'proceed but cancelled')).toBe(false);
  });

  it('an empty gate matches anything', () => {
    expect(m({}, 'whatever')).toBe(true);
    expect(m({}, '')).toBe(true);
  });

  it('compileGate throws on an invalid pattern', () => {
    expect(() => compileGate({ regex: ['([unclosed'] })).toThrow();
  });

  it('compiled gates are reusable across calls', () => {
    const compiled = compileGate({ lineRegex: ['^done$'] });
    expect(matchGate(compiled, 'x\ndone')).toBe(true);
    expect(matchGate(compiled, 'nothing')).toBe(false);
    expect(matchGate(compiled, 'done')).toBe(true);
  });
});
