'use strict';

const fs = require('fs');
const path = require('path');
const { compileManifest } = require('./manifest');

const SOURCE_ORDER = ['user', 'cache', 'bundled'];

function readDirSafe(dir) {
  try {
    return fs.readdirSync(dir).filter(f => f.endsWith('.json'));
  } catch {
    return []; // missing directory is normal, not an error
  }
}

/** Coerce any input to a lowercase string id without ever throwing. */
function normalizeId(agent) {
  return String(agent).toLowerCase();
}

/** Strip a leading UTF-8 BOM (U+FEFF) so hand-saved files still parse. */
function stripBom(text) {
  return text.charCodeAt(0) === 0xFEFF ? text.slice(1) : text;
}

/**
 * Resolve one directory's `.json` files to canonical lowercase agent ids.
 * Filesystems can be case-insensitive (Windows/macOS — a directory can
 * never really hold two dirents differing only by case) or case-sensitive
 * (Linux — it can). Either way we never guess: exactly one file per id is
 * used; two or more distinct filenames collapsing onto the same id is a
 * collision, reported rather than resolved arbitrarily.
 * Returns Map<id, { file }> or Map<id, { collision: string[] }>.
 */
function resolveDirEntries(dir) {
  const byId = new Map(); // id -> filenames[]
  for (const file of readDirSafe(dir)) {
    const id = normalizeId(file.slice(0, -'.json'.length));
    if (!byId.has(id)) byId.set(id, []);
    byId.get(id).push(file);
  }
  const resolved = new Map();
  for (const [id, files] of byId) {
    resolved.set(id, files.length === 1 ? { file: files[0] } : { collision: files.slice().sort() });
  }
  return resolved;
}

/**
 * Manifest registry with source precedence: user override > remote cache >
 * bundled. A malformed file at any level is skipped (and reported) so the
 * next source still applies — detection degrades, never dies. Agent ids are
 * canonicalized to lowercase so a network-synced cache file's casing can
 * never mint a second, phantom agent alongside the bundled one.
 */
function createRegistry({ bundledDir, userDir, cacheDir, log } = {}) {
  const dirs = { user: userDir, cache: cacheDir, bundled: bundledDir };
  let cacheMap = null;   // agent -> { manifest, source, version }
  let lastSkipped = [];

  function loadAll() {
    const result = new Map();
    const skipped = [];
    const agents = new Set();

    // Scan each directory once, resolving filenames to canonical ids.
    const resolutions = {};
    for (const source of SOURCE_ORDER) {
      const dir = dirs[source];
      resolutions[source] = dir ? resolveDirEntries(dir) : new Map();
      for (const id of resolutions[source].keys()) agents.add(id);
    }

    for (const agent of agents) {
      for (const source of SOURCE_ORDER) {
        const dir = dirs[source];
        if (!dir) continue;
        const entry = resolutions[source].get(agent);
        if (!entry) continue; // this source has no file for this agent

        if (entry.collision) {
          const error = `multiple manifest files collide on agent id "${agent}": ${entry.collision.join(', ')}`;
          skipped.push({ agent, source, error });
          if (log) log(`[detect] skipped ${source} manifest for ${agent}: ${error}`);
          continue;
        }

        const file = path.join(dir, entry.file);
        let doc;
        try {
          doc = JSON.parse(stripBom(fs.readFileSync(file, 'utf-8')));
        } catch (err) {
          skipped.push({ agent, source, error: err.message });
          if (log) log(`[detect] skipped ${source} manifest for ${agent}: ${err.message}`);
          continue;
        }
        const res = compileManifest(doc);
        if (!res.ok) {
          skipped.push({ agent, source, error: res.error });
          if (log) log(`[detect] skipped ${source} manifest for ${agent}: ${res.error}`);
          continue;
        }
        result.set(agent, { manifest: res.manifest, source, version: res.manifest.version });
        break; // highest-precedence valid source wins
      }
    }
    lastSkipped = skipped;
    return { result, skipped };
  }

  function ensure() {
    if (!cacheMap) cacheMap = loadAll().result;
    return cacheMap;
  }

  return {
    get(agent) { return ensure().get(normalizeId(agent))?.manifest ?? null; },
    sourceOf(agent) { return ensure().get(normalizeId(agent))?.source ?? null; },
    list() {
      return [...ensure().entries()].map(([agent, e]) => ({
        agent, version: e.version, source: e.source,
      }));
    },
    reload() {
      const { result, skipped } = loadAll();
      cacheMap = result;
      return { loaded: [...result.keys()], skipped };
    },
    lastSkipped() { return lastSkipped.slice(); },
  };
}

module.exports = { createRegistry };
