'use strict';

const { extractRegion } = require('./regions');
const { matchGate } = require('./gates');

const NO_MATCH_REASON = 'no_rule_matched_default_idle';
const EVIDENCE_TEXT_CAP = 400;

/**
 * Evaluate a compiled manifest against a detection snapshot.
 * Rules are pre-sorted by descending priority, so the first match wins.
 * Every rule contributes evidence — detect.explain shows why a rule lost.
 * Never throws, even on malformed snapshots or manifests.
 */
function evaluate(snapshot, manifest) {
  if (!manifest) {
    return { state: 'unknown', ruleId: null, skipStateUpdate: false, reason: 'no_manifest', evidence: [] };
  }

  // Normalize snapshot: null/undefined → {}; non-array rows → []; coerce non-string rows to ''
  let normalizedSnapshot = snapshot;
  if (!snapshot || typeof snapshot !== 'object') {
    normalizedSnapshot = {};
  }
  if (!Array.isArray(normalizedSnapshot.rows)) {
    normalizedSnapshot = { ...normalizedSnapshot, rows: [] };
  } else {
    // Coerce non-string row elements to empty string
    normalizedSnapshot = {
      ...normalizedSnapshot,
      rows: normalizedSnapshot.rows.map(r => (typeof r === 'string' ? r : '')),
    };
  }
  // Coerce non-string title/progress the same way: extractRegion's
  // `snapshot.title || ''` / `snapshot.progress || ''` only substitutes on
  // falsy values, so a truthy non-string (object, number, ...) would flow
  // through as-is into matchGate and, on the osc_title/osc_progress path,
  // into evidenceEntry.regionText's `.slice()` outside the per-rule
  // try/catch below — a real, if currently unreachable, crash risk.
  normalizedSnapshot = {
    ...normalizedSnapshot,
    title: typeof normalizedSnapshot.title === 'string' ? normalizedSnapshot.title : '',
    progress: typeof normalizedSnapshot.progress === 'string' ? normalizedSnapshot.progress : '',
  };

  // Guard rules: if not an array, treat as no-match
  const rules = Array.isArray(manifest.rules) ? manifest.rules : [];

  const evidence = [];
  let winner = null;
  let anyRuleThrew = false;
  for (const rule of rules) {
    let text = '';
    let matched = false;
    let error = undefined;
    try {
      text = extractRegion(normalizedSnapshot, rule.region);
      // Guard gate: if undefined, treat as no-match
      if (rule.gate !== undefined) {
        matched = matchGate(rule.gate, text);
      }
    } catch (e) {
      // Capture error for evidence; mark as evaluation error
      error = String(e);
      anyRuleThrew = true;
      matched = false;
    }
    const evidenceEntry = {
      ruleId: rule.id,
      region: rule.region,
      matched,
      regionText: text.slice(0, EVIDENCE_TEXT_CAP),
    };
    if (error !== undefined) {
      evidenceEntry.error = error;
    }
    evidence.push(evidenceEntry);
    if (matched && !winner) winner = rule;
  }
  if (!winner) {
    const reason = anyRuleThrew ? 'rule_evaluation_error' : NO_MATCH_REASON;
    return { state: 'idle', ruleId: null, skipStateUpdate: false, reason, evidence };
  }
  return {
    state: winner.state,
    ruleId: winner.id,
    skipStateUpdate: winner.skipStateUpdate,
    reason: `rule:${winner.id}`,
    evidence,
  };
}

module.exports = { evaluate, NO_MATCH_REASON };
