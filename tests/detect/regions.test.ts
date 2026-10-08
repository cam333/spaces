import { describe, it, expect } from 'vitest';
import { extractRegion, REGION_NAMES } from '../../bin/lib/runtime/detect/regions.js';

const snap = (rows: string[], extra: Partial<{title: string; progress: string|null}> = {}) => ({
  rows, title: extra.title ?? '', progress: extra.progress ?? null,
  cols: 80, rows_count: rows.length,
});

describe('extractRegion', () => {
  it('returns the OSC title and progress payloads', () => {
    const s = snap(['irrelevant'], { title: '✳ Claude', progress: '4;3;' });
    expect(extractRegion(s, 'osc_title')).toBe('✳ Claude');
    expect(extractRegion(s, 'osc_progress')).toBe('4;3;');
  });

  it('returns empty string when progress was never set', () => {
    expect(extractRegion(snap(['x']), 'osc_progress')).toBe('');
  });

  it('joins all visible rows for whole_visible', () => {
    expect(extractRegion(snap(['a', 'b', 'c']), 'whole_visible')).toBe('a\nb\nc');
  });

  it('bottom_non_empty_lines(N) takes the last N non-blank rows in order', () => {
    const s = snap(['one', '', 'two', 'three', '', '']);
    expect(extractRegion(s, 'bottom_non_empty_lines(2)')).toBe('two\nthree');
    expect(extractRegion(s, 'bottom_non_empty_lines(5)')).toBe('one\ntwo\nthree');
  });

  it('top_non_empty_lines(N) takes the first N non-blank rows in order', () => {
    const s = snap(['', 'alpha', 'beta', '', 'gamma']);
    expect(extractRegion(s, 'top_non_empty_lines(2)')).toBe('alpha\nbeta');
  });

  it('after_last_horizontal_rule returns rows below the last rule line', () => {
    const s = snap(['header', '─────────', 'body one', 'body two']);
    expect(extractRegion(s, 'after_last_horizontal_rule')).toBe('body one\nbody two');
  });

  it('after_last_horizontal_rule falls back to whole_visible with no rule', () => {
    const s = snap(['a', 'b']);
    expect(extractRegion(s, 'after_last_horizontal_rule')).toBe('a\nb');
  });

  it('prompt_box_body returns the interior of the last box-drawn frame', () => {
    const s = snap([
      'chatter above',
      '╭────────────╮',
      '│ > hello    │',
      '│            │',
      '╰────────────╯',
      'status line',
    ]);
    expect(extractRegion(s, 'prompt_box_body')).toBe('> hello');
  });

  it('prompt_box_body returns empty string when no box is present', () => {
    expect(extractRegion(snap(['plain', 'text']), 'prompt_box_body')).toBe('');
  });

  it('last_non_empty_above_prompt_box returns the last non-blank row above the box', () => {
    const s = snap([
      'chatter above',
      '✻ Waiting for 2 background agents to finish',
      '',
      '╭────────────╮',
      '│ ❯          │',
      '╰────────────╯',
    ]);
    expect(extractRegion(s, 'last_non_empty_above_prompt_box'))
      .toBe('✻ Waiting for 2 background agents to finish');
  });

  it('last_non_empty_above_prompt_box is empty with no box, or nothing above it', () => {
    expect(extractRegion(snap(['plain', 'text']), 'last_non_empty_above_prompt_box')).toBe('');
    expect(extractRegion(snap(['╭──╮', '│ ❯ │', '╰──╯']), 'last_non_empty_above_prompt_box')).toBe('');
  });

  it('returns empty string for an unknown region spec', () => {
    expect(extractRegion(snap(['a']), 'no_such_region')).toBe('');
  });

  it('exports the supported region names', () => {
    expect(REGION_NAMES).toContain('osc_title');
    expect(REGION_NAMES).toContain('prompt_box_body');
  });

  it('after_last_horizontal_rule does not treat box borders as rules', () => {
    // Bug fix: box bottom border (╰────────────╯) should NOT be treated as a horizontal rule
    const s = snap([
      'chatter above',
      '╭────────────╮',
      '│ > hello    │',
      '│            │',
      '╰────────────╯',
      'status line',
    ]);
    const result = extractRegion(s, 'after_last_horizontal_rule');
    // Should return all rows (no real rule found), which includes '> hello'
    expect(result).toContain('> hello');
  });

  it('detects horizontal rule rows and rejects vertical-only rows', () => {
    // Real horizontal rule should be detected
    const withRule = snap(['header', '─────────', 'body']);
    expect(extractRegion(withRule, 'after_last_horizontal_rule')).toBe('body');

    // Vertical bars should NOT be treated as a rule
    const withVerticals = snap(['text above', '│   │', 'text below']);
    expect(extractRegion(withVerticals, 'after_last_horizontal_rule')).toContain('text below');
  });

  it('table-driven: each of 9 admitted horizontal glyphs IS treated as a rule', () => {
    // Exact glyphs: ─ ━ ┄ ┅ ┈ ┉ ╌ ╍ ═
    const admittedGlyphs = [
      { char: '─', name: 'U+2500 light horizontal' },
      { char: '━', name: 'U+2501 heavy horizontal' },
      { char: '┄', name: 'U+2504 light triple dash' },
      { char: '┅', name: 'U+2505 heavy triple dash' },
      { char: '┈', name: 'U+2508 light quadruple dash' },
      { char: '┉', name: 'U+2509 heavy quadruple dash' },
      { char: '╌', name: 'U+254C light double dash' },
      { char: '╍', name: 'U+254D heavy double dash' },
      { char: '═', name: 'U+2550 double horizontal' },
    ];

    for (const { char, name } of admittedGlyphs) {
      const row = char.repeat(9);
      const s = snap(['header', row, 'body']);
      expect(extractRegion(s, 'after_last_horizontal_rule'), `${name} should be treated as rule`).toBe('body');
    }
  });

  it('table-driven: rejected glyphs are NOT treated as rules', () => {
    // Explicitly rejected: verticals, corners, junctions, vertical dashes
    const rejectedGlyphs = [
      { char: '┆', name: 'U+2506 vertical triple dash' },
      { char: '┇', name: 'U+2507 vertical quadruple dash' },
      { char: '╎', name: 'U+254E vertical double dash' },
      { char: '╏', name: 'U+254F vertical double dash' },
      { char: '╄', name: 'U+2544 junction' },
      { char: '╅', name: 'U+2545 junction' },
      { char: '╆', name: 'U+2546 junction' },
      { char: '╇', name: 'U+2547 junction' },
      { char: '╈', name: 'U+2548 junction' },
      { char: '╉', name: 'U+2549 junction' },
      { char: '│', name: 'U+2502 vertical' },
      { char: '┃', name: 'U+2503 vertical' },
      { char: '┌', name: 'U+250C corner' },
      { char: '┐', name: 'U+2510 corner' },
      { char: '└', name: 'U+2514 corner' },
      { char: '┘', name: 'U+2518 corner' },
      { char: '╭', name: 'U+256D corner' },
      { char: '╮', name: 'U+256E corner' },
      { char: '╯', name: 'U+256F corner' },
      { char: '╰', name: 'U+2570 corner' },
    ];

    for (const { char, name } of rejectedGlyphs) {
      const row = char.repeat(5);
      const s = snap(['text above', row, 'text below']);
      const result = extractRegion(s, 'after_last_horizontal_rule');
      expect(result, `${name} should NOT be treated as rule`).toContain('text below');
    }
  });

  it('regression: a repro with ╎ and ╉ does not swallow box', () => {
    // Critical: rows with vertical-dashes (╎ U+254E) and junctions (╉ U+2549)
    // should NOT be treated as rules, allowing '> hello' to pass through
    const s = snap([
      'chatter above',
      '╎╎╎╎╎╎',
      '│ > hello    │',
      '│            │',
      '╎╎╎╎╎╎',
      'status line',
    ]);
    const result = extractRegion(s, 'after_last_horizontal_rule');
    // Should return all rows (no real rule found), which includes '> hello'
    expect(result).toContain('> hello');
  });
});
