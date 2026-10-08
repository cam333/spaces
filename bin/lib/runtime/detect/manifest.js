'use strict';

const { z } = require('zod');
const { compileGate } = require('./gates');

const ENGINE_VERSION = 1;
const MAX_GATE_DEPTH = 8;

const GateShape = {
  contains: z.array(z.string()).optional(),
  regex: z.array(z.string()).optional(),
  lineRegex: z.array(z.string()).optional(),
};

// `any`/`all`/`not` nest arbitrarily; z.lazy keeps the schema recursive.
const Gate = z.lazy(() => z.object({
  ...GateShape,
  any: z.array(Gate).optional(),
  all: z.array(Gate).optional(),
  not: z.array(Gate).optional(),
}).strict());

const Rule = z.object({
  id: z.string().min(1),
  state: z.enum(['working', 'blocked', 'idle', 'unknown']),
  priority: z.number().int().nonnegative(),
  region: z.string().min(1),
  skipStateUpdate: z.boolean().optional(),
  ...GateShape,
  any: z.array(Gate).optional(),
  all: z.array(Gate).optional(),
  not: z.array(Gate).optional(),
}).strict();

const ManifestSchema = z.object({
  agent: z.string().min(1),
  version: z.string().min(1),
  minEngineVersion: z.number().int().positive(),
  rules: z.array(Rule).min(1),
}).strict();

/**
 * Check gate nesting depth, return { ok, error } or { ok: true }.
 * Traverse all three arrays independently to find the deepest path.
 */
function checkGateDepth(gate, depth = 0, ruleId = 'unknown') {
  if (depth > MAX_GATE_DEPTH) {
    return { ok: false, error: `rule "${ruleId}": gate nesting exceeds depth ${MAX_GATE_DEPTH}` };
  }
  // Check all three arrays independently
  for (const key of ['any', 'all', 'not']) {
    for (const subgate of gate[key] || []) {
      const check = checkGateDepth(subgate, depth + 1, ruleId);
      if (!check.ok) return check;
    }
  }
  return { ok: true };
}

/**
 * Validate + compile a manifest document. Never throws: every failure mode
 * returns { ok:false, error } so a bad manifest is skipped, not fatal.
 */
function compileManifest(doc) {
  try {
    const parsed = ManifestSchema.safeParse(doc);
    if (!parsed.success) {
      const first = parsed.error.issues[0];
      return { ok: false, error: `${first.path.join('.') || 'manifest'}: ${first.message}` };
    }
    const data = parsed.data;
    if (data.minEngineVersion > ENGINE_VERSION) {
      return {
        ok: false,
        error: `manifest needs engine version ${data.minEngineVersion}, this build is ${ENGINE_VERSION}`,
      };
    }
    const seen = new Set();
    for (const rule of data.rules) {
      if (seen.has(rule.id)) return { ok: false, error: `duplicate rule id "${rule.id}"` };
      seen.add(rule.id);
    }
    const rules = [];
    for (const rule of data.rules) {
      // Check gate nesting depth before compilation
      const depthCheck = checkGateDepth(rule, 0, rule.id);
      if (!depthCheck.ok) return depthCheck;

      let gate;
      try {
        gate = compileGate(rule);
      } catch (err) {
        return { ok: false, error: `rule "${rule.id}": invalid regex — ${err.message}` };
      }
      rules.push({
        id: rule.id,
        state: rule.state,
        priority: rule.priority,
        region: rule.region,
        skipStateUpdate: rule.skipStateUpdate === true,
        gate,
      });
    }
    rules.sort((a, b) => b.priority - a.priority);
    return { ok: true, manifest: { agent: data.agent, version: data.version, rules } };
  } catch (err) {
    // Catch-all for any unguarded error (stack overflow, etc.)
    return { ok: false, error: `manifest compilation failed: ${err.message}` };
  }
}

module.exports = { ManifestSchema, compileManifest, ENGINE_VERSION };
