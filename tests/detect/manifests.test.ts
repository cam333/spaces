// tests/detect/manifests.test.ts
import { describe, it, expect } from 'vitest';
import fs from 'node:fs';
import path from 'node:path';
import { compileManifest } from '../../bin/lib/runtime/detect/manifest.js';
import { evaluate } from '../../bin/lib/runtime/detect/evaluate.js';

const MANIFEST_DIR = path.join(__dirname, '../../bin/lib/runtime/detect/manifests');
const FIXTURE_DIR = path.join(__dirname, 'fixtures');

const manifestFiles = fs.readdirSync(MANIFEST_DIR).filter(f => f.endsWith('.json'));

function loadManifest(agent: string) {
  const doc = JSON.parse(fs.readFileSync(path.join(MANIFEST_DIR, `${agent}.json`), 'utf-8'));
  const res = compileManifest(doc) as any;
  if (!res.ok) throw new Error(`${agent}: ${res.error}`);
  return res.manifest;
}

function allFixtures() {
  const out: Array<{ agent: string; state: string; file: string; data: any }> = [];
  for (const agent of fs.readdirSync(FIXTURE_DIR)) {
    const dir = path.join(FIXTURE_DIR, agent);
    if (!fs.statSync(dir).isDirectory()) continue;
    for (const file of fs.readdirSync(dir).filter(f => f.endsWith('.json'))) {
      out.push({
        agent, state: file.replace(/\.json$/, ''), file,
        data: JSON.parse(fs.readFileSync(path.join(dir, file), 'utf-8')),
      });
    }
  }
  return out;
}

describe('bundled manifests', () => {
  it('ships manifests for the four supported agents', () => {
    expect(manifestFiles.sort()).toEqual(['aider.json', 'claude.json', 'codex.json', 'gemini.json']);
  });

  for (const file of manifestFiles) {
    it(`${file} compiles cleanly`, () => {
      const doc = JSON.parse(fs.readFileSync(path.join(MANIFEST_DIR, file), 'utf-8'));
      const res = compileManifest(doc) as any;
      expect(res.ok, res.error).toBe(true);
      expect(res.manifest.agent).toBe(file.replace(/\.json$/, ''));
    });
  }
});

describe('detection fixtures', () => {
  const fixtures = allFixtures();

  it('has at least fifteen recorded fixtures', () => {
    expect(fixtures.length).toBeGreaterThanOrEqual(15);
  });

  for (const fx of fixtures) {
    it(`${fx.agent}/${fx.file} detects ${fx.data.expectedState}`, () => {
      const manifest = loadManifest(fx.data.agent);
      const res = evaluate(fx.data.snapshot, manifest);
      expect(res.state, `matched rule: ${res.ruleId}`).toBe(fx.data.expectedState);
    });
  }

  it('fixture agent field matches its directory', () => {
    for (const fx of fixtures) expect(fx.data.agent).toBe(fx.agent);
  });
});
