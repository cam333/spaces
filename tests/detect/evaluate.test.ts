import { describe, it, expect } from 'vitest';
import { compileManifest } from '../../bin/lib/runtime/detect/manifest.js';
import { evaluate, NO_MATCH_REASON } from '../../bin/lib/runtime/detect/evaluate.js';

const manifest = (compileManifest({
  agent: 'test', version: '1', minEngineVersion: 1,
  rules: [
    { id: 'blocked_prompt', state: 'blocked', priority: 900, region: 'whole_visible', contains: ['proceed?'] },
    { id: 'title_spinner', state: 'working', priority: 800, region: 'osc_title', regex: ['^[\\u2800-\\u28FF] '] },
    { id: 'viewer', state: 'unknown', priority: 1000, region: 'whole_visible', contains: ['showing transcript'], skipStateUpdate: true },
    { id: 'prompt_idle', state: 'idle', priority: 500, region: 'whole_visible', lineRegex: ['^> $'] },
  ],
}) as any).manifest;

const snap = (rows: string[], title = '') => ({ rows, title, progress: null, cols: 80, rows_count: rows.length });

describe('evaluate', () => {
  it('picks the highest-priority matching rule', () => {
    const res = evaluate(snap(['do you want to proceed?'], '⠋ working'), manifest);
    expect(res.ruleId).toBe('blocked_prompt');
    expect(res.state).toBe('blocked');
  });

  it('matches an OSC title rule when the screen has nothing', () => {
    const res = evaluate(snap(['quiet'], '⠋ thinking'), manifest);
    expect(res.ruleId).toBe('title_spinner');
    expect(res.state).toBe('working');
  });

  it('propagates skipStateUpdate from the winning rule', () => {
    const res = evaluate(snap(['showing transcript', 'do you want to proceed?']), manifest);
    expect(res.ruleId).toBe('viewer');
    expect(res.skipStateUpdate).toBe(true);
  });

  it('falls back to idle with a reason when nothing matches', () => {
    const res = evaluate(snap(['nothing interesting']), manifest);
    expect(res.state).toBe('idle');
    expect(res.ruleId).toBeNull();
    expect(res.reason).toBe(NO_MATCH_REASON);
  });

  it('returns unknown when there is no manifest', () => {
    const res = evaluate(snap(['anything']), null);
    expect(res.state).toBe('unknown');
    expect(res.reason).toBe('no_manifest');
  });

  it('returns evidence for every rule, matched or not', () => {
    const res = evaluate(snap(['do you want to proceed?']), manifest);
    expect(res.evidence).toHaveLength(4);
    const byId = Object.fromEntries(res.evidence.map((e: any) => [e.ruleId, e]));
    expect(byId.blocked_prompt.matched).toBe(true);
    expect(byId.title_spinner.matched).toBe(false);
    expect(byId.blocked_prompt.region).toBe('whole_visible');
    expect(byId.blocked_prompt.regionText).toContain('proceed?');
  });

  it('truncates long region text in evidence', () => {
    const long = 'x'.repeat(1000);
    const res = evaluate(snap([long]), manifest);
    for (const e of res.evidence) expect(e.regionText.length).toBeLessThanOrEqual(400);
  });

  it('handles null snapshot without throwing', () => {
    const res = evaluate(null, manifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(4);
  });

  it('handles undefined snapshot without throwing', () => {
    const res = evaluate(undefined, manifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(4);
  });

  it('handles empty object snapshot without throwing', () => {
    const res = evaluate({}, manifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(4);
  });

  it('handles non-array rows without throwing', () => {
    const res = evaluate({ rows: 'notanarray', title: '', progress: null, cols: 80 }, manifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(4);
  });

  it('handles missing rules array without throwing', () => {
    const badManifest = { agent: 'test', version: '1', rules: undefined };
    const res = evaluate(snap(['anything']), badManifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(0);
  });

  it('handles rule with undefined gate without throwing', () => {
    const badManifest = {
      agent: 'test',
      version: '1',
      rules: [
        { id: 'broken', state: 'working', priority: 100, region: 'whole_visible', gate: undefined },
      ],
    };
    const res = evaluate(snap(['anything']), badManifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(1);
    expect(res.evidence[0].matched).toBe(false);
    expect(res.evidence[0].error).toBeUndefined();
  });

  it('coerces non-string rows to empty string and handles cleanly', () => {
    // Snapshot with null element in rows that would cause extractRegion to throw
    const res = evaluate({ rows: [null, 'hello'], title: '', progress: null, cols: 80 }, manifest);
    expect(res.state).toBe('idle');
    // Should not throw and should not have error field (rows were coerced)
    expect(res.evidence).toHaveLength(4);
    for (const e of res.evidence) {
      expect(e.error).toBeUndefined();
    }
  });

  it('reports rule_evaluation_error reason when gate deliberately throws', () => {
    // Create a gate that will throw when matchGate tries to process it
    // Use a non-iterable value for a property that matchGate tries to iterate
    const badManifest = {
      agent: 'test',
      version: '1',
      rules: [
        { id: 'bad_gate', state: 'working', priority: 100, region: 'whole_visible', gate: { regex: 123 } },
      ],
    };
    const res = evaluate(snap(['anything']), badManifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe('rule_evaluation_error');
    expect(res.evidence).toHaveLength(1);
    expect(res.evidence[0].matched).toBe(false);
    expect(res.evidence[0].error).toBeDefined();
    expect(typeof res.evidence[0].error).toBe('string');
  });

  it('coerces a truthy non-string title and does not throw', () => {
    // extractRegion's `snapshot.title || ''` only substitutes on falsy
    // values, so a truthy non-string title would otherwise flow through
    // into matchGate's toLowerCase() and, on a bad-gate rule, into
    // evidenceEntry.regionText's slice() outside the per-rule try/catch.
    const res = evaluate({ rows: ['quiet'], title: 12345, progress: null, cols: 80 }, manifest);
    expect(res.state).toBe('idle');
    expect(res.evidence).toHaveLength(4);
    for (const e of res.evidence) expect(e.error).toBeUndefined();
  });

  it('coerces a truthy non-string progress and does not throw', () => {
    const res = evaluate({ rows: ['quiet'], title: '', progress: { weird: true }, cols: 80 }, manifest);
    expect(res.state).toBe('idle');
    expect(res.evidence).toHaveLength(4);
    for (const e of res.evidence) expect(e.error).toBeUndefined();
  });

  it('a well-formed result still comes back for a non-string title matching an osc_title rule', () => {
    // title_spinner matches osc_title with a regex; a non-string title must
    // not silently prevent that region from ever being usable — it should
    // just coerce to '' and fail to match like any other empty title would.
    const res = evaluate({ rows: ['quiet'], title: 42, progress: null, cols: 80 }, manifest);
    expect(res.ruleId).not.toBe('title_spinner');
    expect(typeof res.state).toBe('string');
  });

  it('normal no-match still has clean NO_MATCH_REASON and no error fields', () => {
    const res = evaluate(snap(['nothing at all']), manifest);
    expect(res.state).toBe('idle');
    expect(res.reason).toBe(NO_MATCH_REASON);
    expect(res.evidence).toHaveLength(4);
    for (const e of res.evidence) {
      expect(e.error).toBeUndefined();
    }
  });
});
