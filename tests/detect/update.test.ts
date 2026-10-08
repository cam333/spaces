// tests/detect/update.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { refreshManifests, MAX_MANIFEST_BYTES } from '../../bin/lib/runtime/detect/update.js';
import { createRegistry } from '../../bin/lib/runtime/detect/registry.js';

let root: string, bundledDir: string, cacheDir: string;

const goodDoc = (agent: string, version: string) => ({
  agent, version, minEngineVersion: 1,
  rules: [{ id: 'r', state: 'idle', priority: 10, region: 'whole_visible', contains: ['x'] }],
});

function fakeFetch(routes: Record<string, { body: string; status?: number }>) {
  return async (url: string) => {
    const hit = routes[url];
    if (!hit) return { ok: false, status: 404, text: async () => '' };
    return {
      ok: (hit.status ?? 200) < 400,
      status: hit.status ?? 200,
      text: async () => hit.body,
    };
  };
}

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-upd-'));
  bundledDir = path.join(root, 'bundled');
  cacheDir = path.join(root, 'cache');
  fs.mkdirSync(bundledDir, { recursive: true });
  fs.writeFileSync(path.join(bundledDir, 'claude.json'), JSON.stringify(goodDoc('claude', 'bundled-1')));
});

afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

const INDEX = 'https://example.test/detection/index.json';

describe('refreshManifests', () => {
  it('downloads and caches a newer manifest, then the registry serves it', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'remote-2', url: 'https://example.test/claude.json' }] }) },
        'https://example.test/claude.json': { body: JSON.stringify(goodDoc('claude', 'remote-2')) },
      }),
    });
    expect(res.updated).toEqual(['claude']);
    expect(registry.get('claude').version).toBe('remote-2');
    expect(registry.sourceOf('claude')).toBe('cache');
  });

  it('skips an entry whose version already matches the cache', async () => {
    fs.mkdirSync(cacheDir, { recursive: true });
    fs.writeFileSync(path.join(cacheDir, 'claude.json'), JSON.stringify(goodDoc('claude', 'remote-2')));
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'remote-2', url: 'https://example.test/claude.json' }] }) },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped.some(s => s.agent === 'claude' && /up.to.date/i.test(s.reason))).toBe(true);
  });

  it('skips an index that needs a newer engine', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 99, manifests: [{ agent: 'claude', version: 'x', url: 'https://example.test/claude.json' }] }) },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/engine/i);
  });

  it('rejects an oversized manifest body', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const huge = 'x'.repeat(MAX_MANIFEST_BYTES + 1);
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'remote-3', url: 'https://example.test/claude.json' }] }) },
        'https://example.test/claude.json': { body: huge },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/too large|size/i);
    expect(fs.existsSync(path.join(cacheDir, 'claude.json'))).toBe(false);
  });

  it('rejects a manifest that fails validation and leaves the cache untouched', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'remote-4', url: 'https://example.test/claude.json' }] }) },
        'https://example.test/claude.json': { body: JSON.stringify({ agent: 'claude', version: 'remote-4', minEngineVersion: 1, rules: [] }) },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(fs.existsSync(path.join(cacheDir, 'claude.json'))).toBe(false);
    expect(registry.get('claude').version).toBe('bundled-1');
  });

  it('resolves rather than rejecting when the index fetch fails', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: async () => { throw new Error('offline'); },
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/offline|fetch/i);
  });

  it('ignores an index entry whose agent name does not match the document', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'v', url: 'https://example.test/other.json' }] }) },
        'https://example.test/other.json': { body: JSON.stringify(goodDoc('codex', 'v')) },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/mismatch/i);
  });

  // --- Security review follow-up: path traversal (CRITICAL) ---------------

  it('rejects path-traversal and malformed agent ids without writing anywhere', async () => {
    // Use a cacheDir nested two levels inside our own sandboxed `root`
    // (rather than the top-level `cacheDir` fixture) so "two directories
    // above cacheDir" — the exact location the reported exploit wrote to —
    // resolves to a path we fully own and that afterEach() cleans up,
    // instead of touching the shared OS temp directory.
    const nestedRoot = path.join(root, 'nested');
    const nestedCacheDir = path.join(nestedRoot, 'sub', 'cache');
    const registry = createRegistry({ bundledDir, cacheDir: nestedCacheDir });
    const maliciousAgents = [
      '../../evil',
      '..\\..\\evil',
      'a/b',
      '/abs/path',
      'C:\\win\\path',
      'x'.repeat(500),
    ];
    const manifests = maliciousAgents.map((agent, i) => ({ agent, version: 'v1', url: `https://example.test/bad-${i}.json` }));
    const routes: Record<string, { body: string }> = {
      [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests }) },
    };
    maliciousAgents.forEach((agent, i) => {
      routes[`https://example.test/bad-${i}.json`] = { body: JSON.stringify(goodDoc(agent, 'v1')) };
    });

    const res = await refreshManifests({ cacheDir: nestedCacheDir, registry, indexUrl: INDEX, fetchImpl: fakeFetch(routes) });

    expect(res.updated).toEqual([]);
    expect(res.skipped.length).toBe(maliciousAgents.length);
    for (const s of res.skipped) expect(s.reason).toMatch(/invalid agent/i);

    // Nothing escaped two directories above cacheDir (== nestedRoot), and
    // cacheDir itself was never even created since every entry was rejected
    // before any write.
    expect(fs.existsSync(path.join(nestedRoot, 'evil.json'))).toBe(false);
    expect(fs.existsSync(path.join(nestedRoot, 'evil-implant.json'))).toBe(false);
    expect(fs.existsSync(nestedCacheDir)).toBe(false);
  });

  it('rejects an index entry whose agent name matches the document but is still a path-traversal payload', async () => {
    // The originally reported exploit: the index entry's `agent` and the
    // fetched document's `agent` field agree (so the naive mismatch check
    // alone would not catch it) but the value itself is a traversal payload.
    const nestedRoot = path.join(root, 'nested2');
    const nestedCacheDir = path.join(nestedRoot, 'sub', 'cache');
    const registry = createRegistry({ bundledDir, cacheDir: nestedCacheDir });
    const evilAgent = '../../evil-implant';
    const res = await refreshManifests({
      cacheDir: nestedCacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: evilAgent, version: 'v1', url: 'https://example.test/evil.json' }] }) },
        'https://example.test/evil.json': { body: JSON.stringify(goodDoc(evilAgent, 'v1')) },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/invalid agent/i);
    expect(fs.existsSync(path.join(nestedRoot, 'evil-implant.json'))).toBe(false);
  });

  // --- Security review follow-up: never-rejects invariant (IMPORTANT) -----

  it.each(['null', '[]', '"str"'])('resolves with a skipped entry when the index body is %s', async (body) => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({ [INDEX]: { body } }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped.length).toBeGreaterThan(0);
  });

  it('reports a distinct reason when the index body is not valid JSON at all', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({ [INDEX]: { body: 'not json {' } }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/index parse failed/i);
  });

  it('skips the index when engineVersion is missing or non-numeric instead of silently passing the gate', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({ [INDEX]: { body: JSON.stringify({ manifests: [] }) } }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/engine/i);
  });

  // --- Security review follow-up: https-only (IMPORTANT) ------------------

  it('rejects non-https manifest urls without ever fetching them', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const calls: string[] = [];
    const trackingFetch = (routes: Record<string, { body: string }>) => {
      const inner = fakeFetch(routes);
      return async (url: string) => { calls.push(url); return inner(url); };
    };
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: trackingFetch({
        [INDEX]: {
          body: JSON.stringify({
            engineVersion: 1,
            manifests: [
              { agent: 'claude', version: 'v1', url: 'http://example.test/claude.json' },
              { agent: 'codex', version: 'v1', url: 'file:///etc/passwd' },
            ],
          }),
        },
        'http://example.test/claude.json': { body: JSON.stringify(goodDoc('claude', 'v1')) },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped.find(s => s.agent === 'claude')?.reason).toMatch(/https/i);
    expect(res.skipped.find(s => s.agent === 'codex')?.reason).toMatch(/https/i);
    // Only the index itself was ever fetched — neither non-https manifest URL was.
    expect(calls).toEqual([INDEX]);
  });

  it('rejects a non-https index url without fetching it', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    const calls: string[] = [];
    const trackingFetch = (routes: Record<string, { body: string }>) => {
      const inner = fakeFetch(routes);
      return async (url: string) => { calls.push(url); return inner(url); };
    };
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: 'http://example.test/detection/index.json',
      fetchImpl: trackingFetch({}),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/https/i);
    expect(calls).toEqual([]);
  });

  // --- Security review follow-up: minors -----------------------------------

  it('rejects an oversized manifest body measured in bytes, not UTF-16 code units', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    // '中' is 1 UTF-16 code unit but 3 UTF-8 bytes. 100_000 code units stays
    // under the (code-unit) length cap that a naive `.length` check would
    // apply, but its 300_000 UTF-8 bytes exceed MAX_MANIFEST_BYTES.
    const multibyte = '中'.repeat(100_000);
    expect(multibyte.length).toBeLessThan(MAX_MANIFEST_BYTES);
    const res = await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'remote-mb', url: 'https://example.test/claude.json' }] }) },
        'https://example.test/claude.json': { body: multibyte },
      }),
    });
    expect(res.updated).toEqual([]);
    expect(res.skipped[0].reason).toMatch(/too large|size/i);
    expect(fs.existsSync(path.join(cacheDir, 'claude.json'))).toBe(false);
  });

  it('writes the cache file atomically, leaving no temp file debris', async () => {
    const registry = createRegistry({ bundledDir, cacheDir });
    await refreshManifests({
      cacheDir, registry, indexUrl: INDEX,
      fetchImpl: fakeFetch({
        [INDEX]: { body: JSON.stringify({ engineVersion: 1, manifests: [{ agent: 'claude', version: 'remote-atomic', url: 'https://example.test/claude.json' }] }) },
        'https://example.test/claude.json': { body: JSON.stringify(goodDoc('claude', 'remote-atomic')) },
      }),
    });
    expect(fs.readdirSync(cacheDir)).toEqual(['claude.json']);
  });
});
