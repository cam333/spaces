'use strict';

/**
 * Pre-compile a gate's patterns. Throws on an invalid RegExp so manifest
 * validation can reject the whole manifest rather than failing at match time.
 */
function compileGate(gate) {
  const g = gate || {};
  return {
    contains: (g.contains || []).map(s => String(s).toLowerCase()),
    regex: (g.regex || []).map(p => new RegExp(p)), // Throws on invalid
    lineRegex: (g.lineRegex || []).map(p => new RegExp(p)),
    any: (g.any || []).map(compileGate),
    all: (g.all || []).map(compileGate),
    not: (g.not || []).map(compileGate),
  };
}

/** Evaluate a compiled gate against region text. All present keys must pass. */
function matchGate(compiled, text) {
  const lower = text.toLowerCase();

  for (const needle of compiled.contains) {
    if (!lower.includes(needle)) return false;
  }
  for (const re of compiled.regex) {
    if (!re.test(text)) return false;
  }
  if (compiled.lineRegex.length) {
    const lines = text.split('\n');
    for (const re of compiled.lineRegex) {
      if (!lines.some(line => re.test(line))) return false;
    }
  }
  if (compiled.any.length && !compiled.any.some(sub => matchGate(sub, text))) return false;
  for (const sub of compiled.all) {
    if (!matchGate(sub, text)) return false;
  }
  for (const sub of compiled.not) {
    if (matchGate(sub, text)) return false;
  }
  return true;
}

module.exports = { compileGate, matchGate };
