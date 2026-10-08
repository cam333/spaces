import { describe, it, expect } from 'vitest';
import { compileManifest, ENGINE_VERSION } from '../../bin/lib/runtime/detect/manifest.js';

const base = {
  agent: 'claude', version: '2026.08.07.1', minEngineVersion: 1,
  rules: [
    { id: 'low', state: 'idle', priority: 100, region: 'osc_title', contains: ['ready'] },
    { id: 'high', state: 'blocked', priority: 900, region: 'whole_visible', contains: ['proceed?'] },
  ],
};

describe('compileManifest', () => {
  it('accepts a valid manifest and sorts rules by descending priority', () => {
    const res = compileManifest(base) as any;
    expect(res.ok).toBe(true);
    expect(res.manifest.agent).toBe('claude');
    expect(res.manifest.rules.map((r: any) => r.id)).toEqual(['high', 'low']);
    expect(res.manifest.rules[0].gate).toBeDefined();
  });

  it('defaults skipStateUpdate to false', () => {
    const res = compileManifest(base) as any;
    expect(res.manifest.rules[0].skipStateUpdate).toBe(false);
  });

  it('rejects an unknown state', () => {
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], state: 'busy' }] }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/state/i);
  });

  it('rejects a manifest requiring a newer engine', () => {
    const res = compileManifest({ ...base, minEngineVersion: ENGINE_VERSION + 1 }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/engine/i);
  });

  it('rejects duplicate rule ids', () => {
    const dup = { ...base, rules: [base.rules[0], { ...base.rules[1], id: 'low' }] };
    const res = compileManifest(dup) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/duplicate/i);
  });

  it('rejects an invalid regex without throwing', () => {
    const bad = { ...base, rules: [{ ...base.rules[0], regex: ['([unclosed'] }] };
    const res = compileManifest(bad) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/regex|pattern|invalid/i);
  });

  it('rejects a non-object input without throwing', () => {
    expect((compileManifest(null) as any).ok).toBe(false);
    expect((compileManifest('nope') as any).ok).toBe(false);
  });

  it('accepts nested any/all/not gates', () => {
    const nested = { ...base, rules: [{
      id: 'n', state: 'blocked', priority: 500, region: 'whole_visible',
      contains: ['proceed'],
      any: [{ contains: ['yes'] }, { lineRegex: ['^> '] }],
      not: [{ contains: ['cancelled'] }],
    }] };
    expect((compileManifest(nested) as any).ok).toBe(true);
  });

  it('rejects deeply nested gates exceeding depth 8 without throwing', () => {
    // Build a gate nested 9 levels deep
    const deepGate = { contains: ['x'] };
    let nested = deepGate;
    for (let i = 0; i < 9; i++) {
      nested = { any: [nested] };
    }
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], ...nested }] }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/depth/i);
  });

  it('accepts gates nested exactly 8 levels deep', () => {
    // Build a gate nested 8 levels deep
    let nested = { contains: ['x'] };
    for (let i = 0; i < 8; i++) {
      nested = { any: [nested] };
    }
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], ...nested }] }) as any;
    expect(res.ok).toBe(true);
  });

  it('rejects extremely deep nesting (2000 levels) without throwing', () => {
    // Build a massively deep gate
    let nested = { contains: ['x'] };
    for (let i = 0; i < 2000; i++) {
      nested = { any: [nested] };
    }
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], ...nested }] }) as any;
    expect(res.ok).toBe(false);
    // Should fail either on depth check or catch-all error handling
  });

  it('rejects unknown keys in nested gates', () => {
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], any: [{ bogusKey: 'wat' }] }] }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/bogusKey|unknown|property/i);
  });

  it('rejects misspelled rule-level gate keys', () => {
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], conatins: ['typo'] }] }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/conatins|unknown|property/i);
  });

  it('rejects misspelled rule-level keys like skipStateUpdte', () => {
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], skipStateUpdte: true }] }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/skipStateUpdte|unknown|property/i);
  });

  it('rejects null entries in rules array', () => {
    const res = compileManifest({ ...base, rules: [null] }) as any;
    expect(res.ok).toBe(false);
  });

  it('rejects non-string elements in contains array', () => {
    const res = compileManifest({ ...base, rules: [{ ...base.rules[0], contains: ['valid', 123] }] }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/contains|string|type/i);
  });

  it('rejects any/all mix where all exceeds depth (any shallow + all 9-level)', () => {
    // Build a 9-level chain for the all array
    let deepAll = { contains: ['x'] };
    for (let i = 0; i < 9; i++) {
      deepAll = { all: [deepAll] };
    }
    // Combine with a shallow any
    const res = compileManifest({
      ...base,
      rules: [{
        ...base.rules[0],
        any: [{ contains: ['shallow'] }],
        ...deepAll,
      }],
    }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/depth/i);
  });

  it('rejects any:[] + all 9-level (empty array should not block traversal)', () => {
    // Build a 9-level chain for the all array
    let deepAll = { contains: ['x'] };
    for (let i = 0; i < 9; i++) {
      deepAll = { all: [deepAll] };
    }
    const res = compileManifest({
      ...base,
      rules: [{
        ...base.rules[0],
        any: [],
        ...deepAll,
      }],
    }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/depth/i);
  });

  it('rejects all:[] + not 9-level (empty array should not block traversal)', () => {
    // Build a 9-level chain for the not array
    let deepNot = { contains: ['x'] };
    for (let i = 0; i < 9; i++) {
      deepNot = { not: [deepNot] };
    }
    const res = compileManifest({
      ...base,
      rules: [{
        ...base.rules[0],
        all: [],
        ...deepNot,
      }],
    }) as any;
    expect(res.ok).toBe(false);
    expect(res.error).toMatch(/depth/i);
  });

  it('accepts gate combining any + not within depth 8', () => {
    // An example that combines contains, any, and not
    const res = compileManifest({
      ...base,
      rules: [{
        id: 'combined',
        state: 'blocked',
        priority: 500,
        region: 'whole_visible',
        contains: ['proceed'],
        any: [{ contains: ['yes'] }, { lineRegex: ['^> '] }],
        not: [{ contains: ['cancelled'] }],
      }],
    }) as any;
    expect(res.ok).toBe(true);
  });
});
