'use strict';

const REGION_NAMES = [
  'osc_title', 'osc_progress', 'whole_visible',
  'bottom_non_empty_lines', 'top_non_empty_lines',
  'after_last_horizontal_rule', 'prompt_box_body',
  'last_non_empty_above_prompt_box',
];

// A "horizontal rule" is a row made only of horizontal box-drawing glyphs
// (─ ━ ┄ ┅ ┈ ┉ ╌ ╍ ═), dashes, underscores, equals, and spaces.
// Agents draw these to separate transcript from the live prompt area.
// Explicitly includes ONLY these nine horizontal glyphs (no ranges, no junctions,
// no verticals):
//   ─ (U+2500), ━ (U+2501), ┄ (U+2504), ┅ (U+2505), ┈ (U+2508), ┉ (U+2509),
//   ╌ (U+254C), ╍ (U+254D), ═ (U+2550)
// Plus ASCII dashes, underscores, equals, and whitespace.
const RULE_RE = /^[\s─━┄┅┈┉╌╍═\-_=]+$/;
const BOX_TOP_RE = /^\s*[┌╭][─━]*[┐╮]?/;
const BOX_BOTTOM_RE = /^\s*[└╰][─━]*[┘╯]?/;

function nonEmpty(rows) {
  return rows.filter(r => r.trim() !== '');
}

function isRule(row) {
  return row.trim() !== '' && RULE_RE.test(row);
}

/** Row indexes of the last box-drawn frame's borders, or null without one. */
function promptBoxBounds(rows) {
  let bottom = -1;
  for (let i = rows.length - 1; i >= 0; i--) {
    if (BOX_BOTTOM_RE.test(rows[i])) { bottom = i; break; }
  }
  if (bottom < 0) return null;
  let top = -1;
  for (let i = bottom - 1; i >= 0; i--) {
    if (BOX_TOP_RE.test(rows[i])) { top = i; break; }
  }
  if (top < 0) return null;
  return { top, bottom };
}

/** Interior of the last box-drawn frame in the visible rows. */
function promptBoxBody(rows) {
  const bounds = promptBoxBounds(rows);
  if (!bounds) return '';
  const body = rows.slice(bounds.top + 1, bounds.bottom)
    // strip the vertical border glyphs and surrounding padding
    .map(r => r.replace(/^\s*[│┃]?\s?/, '').replace(/\s*[│┃]\s*$/, '').trimEnd());
  return nonEmpty(body).join('\n');
}

/**
 * The last non-blank row above the last box-drawn frame — where agents
 * park a one-line status ("Waiting for 2 background agents to finish")
 * while the prompt box below already shows an idle-looking ❯.
 */
function lastNonEmptyAbovePromptBox(rows) {
  const bounds = promptBoxBounds(rows);
  if (!bounds) return '';
  for (let i = bounds.top - 1; i >= 0; i--) {
    if (rows[i].trim() !== '') return rows[i];
  }
  return '';
}

/**
 * Extract one named region's text from a detection snapshot.
 * Unknown or inapplicable regions yield '' so a rule simply fails to match
 * rather than throwing.
 */
function extractRegion(snapshot, spec) {
  const rows = snapshot.rows || [];
  const paramMatch = /^([a-z_]+)\((\d+)\)$/.exec(spec);
  const name = paramMatch ? paramMatch[1] : spec;
  const n = paramMatch ? parseInt(paramMatch[2], 10) : null;

  switch (name) {
    case 'osc_title':
      return snapshot.title || '';
    case 'osc_progress':
      return snapshot.progress || '';
    case 'whole_visible':
      return rows.join('\n');
    case 'bottom_non_empty_lines': {
      const ne = nonEmpty(rows);
      return ne.slice(Math.max(0, ne.length - (n ?? 0))).join('\n');
    }
    case 'top_non_empty_lines':
      return nonEmpty(rows).slice(0, n ?? 0).join('\n');
    case 'after_last_horizontal_rule': {
      let last = -1;
      for (let i = rows.length - 1; i >= 0; i--) {
        if (isRule(rows[i])) { last = i; break; }
      }
      if (last < 0) return rows.join('\n');
      return nonEmpty(rows.slice(last + 1)).join('\n');
    }
    case 'prompt_box_body':
      return promptBoxBody(rows);
    case 'last_non_empty_above_prompt_box':
      return lastNonEmptyAbovePromptBox(rows);
    default:
      return '';
  }
}

module.exports = { extractRegion, REGION_NAMES };
