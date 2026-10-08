'use strict';

const fs = require('fs');
const path = require('path');
const { compileManifest, ENGINE_VERSION } = require('./manifest');

const DEFAULT_INDEX_URL = 'https://www.agentspaces.co/detection/index.json';
const MAX_MANIFEST_BYTES = 262144;
const MAX_INDEX_BYTES = 65536;

// Agent ids are used to build cache file paths (`<cacheDir>/<agent>.json`).
// Both the index entry's `agent` field and the fetched document's `agent`
// field are attacker-controlled (a compromised/misconfigured index host can
// set either to anything), so both must be validated against a strict
// allowlist BEFORE ever touching the filesystem — anything else is a path
// traversal / arbitrary-write primitive (e.g. `../../evil-implant`).
const AGENT_ID_PATTERN = /^[a-z0-9_-]+$/i;
const MAX_AGENT_ID_LENGTH = 64;

function isValidAgentId(agent) {
  return typeof agent === 'string'
    && agent.length > 0
    && agent.length <= MAX_AGENT_ID_LENGTH
    && AGENT_ID_PATTERN.test(agent);
}

/** Truncate an untrusted value for inclusion in a skip reason / log line. */
function describeValue(value, max = 80) {
  const str = String(value);
  return str.length > max ? `${str.slice(0, max)}…(${str.length} chars)` : str;
}

/** Only `https:` is trusted for both the index and every manifest URL — this
 * closes off a compromised/misconfigured index host redirecting fetches to
 * `http:`/`file:`/other schemes, which matters doubly in combination with
 * the agent-id path checks above (both were needed for a full arbitrary-write
 * chain). */
function isHttpsUrl(value) {
  if (typeof value !== 'string') return false;
  try {
    return new URL(value).protocol === 'https:';
  } catch {
    return false;
  }
}

async function fetchText(fetchImpl, url, cap) {
  const res = await fetchImpl(url);
  if (!res.ok) throw new Error(`fetch failed: HTTP ${res.status}`);
  const body = await res.text();
  // `.length` counts UTF-16 code units, not bytes — a multibyte manifest
  // could pass a code-unit check yet exceed the byte cap on disk. Measure
  // actual UTF-8 bytes, which is what will actually be written.
  const bytes = Buffer.byteLength(body, 'utf8');
  if (bytes > cap) throw new Error(`body too large (${bytes} > ${cap})`);
  return body;
}

/**
 * Fetch + parse the index, distinguishing network/size failures from a body
 * that parses but isn't a usable object (or doesn't parse at all) — each
 * gets its own reason so a skip is actionable rather than a generic "fetch
 * failed" for what was actually a malformed body.
 */
async function fetchIndex(fetchImpl, indexUrl) {
  let raw;
  try {
    raw = await fetchText(fetchImpl, indexUrl, MAX_INDEX_BYTES);
  } catch (err) {
    return { ok: false, reason: `index fetch failed: ${err.message}` };
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    return { ok: false, reason: `index parse failed: ${err.message}` };
  }
  // A syntactically valid JSON document can still be `null`, an array, or a
  // bare string/number — none of those have the shape we need, and reading
  // `.engineVersion`/`.manifests` off them must never throw (a `null` body
  // previously threw a TypeError here and rejected the whole promise).
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    const kind = parsed === null ? 'null' : Array.isArray(parsed) ? 'array' : typeof parsed;
    return { ok: false, reason: `index body is not a JSON object (got ${kind})` };
  }
  return { ok: true, index: parsed };
}

function cachedVersion(cacheDir, agent) {
  if (!isValidAgentId(agent)) return null;
  try {
    return JSON.parse(fs.readFileSync(path.join(cacheDir, `${agent}.json`), 'utf-8')).version ?? null;
  } catch {
    return null;
  }
}

/** Write the cache file atomically: write to a sibling temp file, then
 * rename over the target, so a crash/interruption mid-write can never leave
 * a truncated manifest where the registry expects a complete one. */
function writeCacheFileAtomic(cacheDir, agent, doc) {
  fs.mkdirSync(cacheDir, { recursive: true });
  const target = path.join(cacheDir, `${agent}.json`);
  const tmp = path.join(cacheDir, `.${agent}.json.${process.pid}.${Date.now()}.tmp`);
  fs.writeFileSync(tmp, JSON.stringify(doc));
  try {
    fs.renameSync(tmp, target);
  } catch (err) {
    try { fs.unlinkSync(tmp); } catch { /* best-effort cleanup */ }
    throw err;
  }
}

/**
 * Refresh cached manifests from the published index. Never rejects: a failed
 * refresh leaves the previous manifests in place, because stale detection
 * beats no detection.
 */
async function refreshManifests({
  cacheDir, registry, indexUrl = DEFAULT_INDEX_URL,
  fetchImpl = globalThis.fetch, log,
} = {}) {
  const updated = [];
  const skipped = [];

  if (!isHttpsUrl(indexUrl)) {
    skipped.push({ agent: '*', reason: `index url must use https: (got "${describeValue(indexUrl)}")` });
    return { updated, skipped };
  }

  const indexResult = await fetchIndex(fetchImpl, indexUrl);
  if (!indexResult.ok) {
    skipped.push({ agent: '*', reason: indexResult.reason });
    if (log) log(`[detect] manifest index unavailable: ${indexResult.reason}`);
    return { updated, skipped };
  }
  const index = indexResult.index;

  // `Number(missing/non-numeric)` is NaN, and `NaN > ENGINE_VERSION` is
  // always false — that silently let an index with no (or a garbage)
  // engineVersion field sail past the gate entirely. Require an actual
  // finite number and skip the index otherwise.
  const engineVersion = Number(index.engineVersion);
  if (!Number.isFinite(engineVersion)) {
    skipped.push({ agent: '*', reason: `index engineVersion is missing or invalid (got ${JSON.stringify(index.engineVersion)})` });
    return { updated, skipped };
  }
  if (engineVersion > ENGINE_VERSION) {
    skipped.push({ agent: '*', reason: `index needs engine ${engineVersion}, this build is ${ENGINE_VERSION}` });
    return { updated, skipped };
  }

  for (const entry of Array.isArray(index.manifests) ? index.manifests : []) {
    const { agent, version, url } = entry || {};
    if (!agent || !version || !url) {
      skipped.push({ agent: agent || '?', reason: 'malformed index entry' });
      continue;
    }
    // Validate BEFORE the agent id is used in any path (cachedVersion below,
    // and the cache write further down) — this is the primary defense
    // against a path-traversal / arbitrary-write index entry.
    if (!isValidAgentId(agent)) {
      skipped.push({ agent, reason: `invalid agent id "${describeValue(agent)}"` });
      continue;
    }
    if (!isHttpsUrl(url)) {
      skipped.push({ agent, reason: `manifest url must use https: (got "${describeValue(url)}")` });
      continue;
    }
    if (cachedVersion(cacheDir, agent) === version) {
      skipped.push({ agent, reason: 'up-to-date' });
      continue;
    }
    let body;
    try {
      body = await fetchText(fetchImpl, url, MAX_MANIFEST_BYTES);
    } catch (err) {
      skipped.push({ agent, reason: err.message });
      continue;
    }
    let doc;
    try {
      doc = JSON.parse(body);
    } catch (err) {
      skipped.push({ agent, reason: `invalid json: ${err.message}` });
      continue;
    }
    // The document's own `agent` field is just as attacker-controlled as the
    // index entry's — apply the same allowlist to it too, in addition to
    // requiring it match the (already-validated) index entry.
    if (!isValidAgentId(doc.agent) || doc.agent !== agent) {
      skipped.push({ agent, reason: `agent mismatch: document declares "${describeValue(doc.agent)}"` });
      continue;
    }
    const compiled = compileManifest(doc);
    if (!compiled.ok) {
      skipped.push({ agent, reason: compiled.error });
      continue;
    }
    try {
      writeCacheFileAtomic(cacheDir, agent, doc);
      updated.push(agent);
    } catch (err) {
      skipped.push({ agent, reason: `cache write failed: ${err.message}` });
    }
  }

  if (updated.length && registry) registry.reload();
  return { updated, skipped };
}

module.exports = { refreshManifests, DEFAULT_INDEX_URL, MAX_MANIFEST_BYTES };
