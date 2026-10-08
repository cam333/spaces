// tests/detect/registry.test.ts
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRegistry } from '../../bin/lib/runtime/detect/registry.js';

let root: string, bundledDir: string, userDir: string, cacheDir: string;

const manifestDoc = (agent: string, version: string, state = 'working') => ({
  agent, version, minEngineVersion: 1,
  rules: [{ id: 'r1', state, priority: 100, region: 'whole_visible', contains: ['x'] }],
});

const write = (dir: string, agent: string, doc: unknown) => {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, `${agent}.json`), JSON.stringify(doc));
};

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-reg-'));
  bundledDir = path.join(root, 'bundled');
  userDir = path.join(root, 'user');
  cacheDir = path.join(root, 'cache');
  write(bundledDir, 'claude', manifestDoc('claude', 'bundled-1'));
});

afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

describe('createRegistry', () => {
  it('loads the bundled manifest when nothing overrides it', () => {
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('claude').version).toBe('bundled-1');
    expect(reg.sourceOf('claude')).toBe('bundled');
  });

  it('prefers the remote cache over bundled', () => {
    write(cacheDir, 'claude', manifestDoc('claude', 'cache-2'));
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('claude').version).toBe('cache-2');
    expect(reg.sourceOf('claude')).toBe('cache');
  });

  it('prefers the user override over everything', () => {
    write(cacheDir, 'claude', manifestDoc('claude', 'cache-2'));
    write(userDir, 'claude', manifestDoc('claude', 'user-3'));
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('claude').version).toBe('user-3');
    expect(reg.sourceOf('claude')).toBe('user');
  });

  it('skips a malformed higher-precedence file and falls back', () => {
    fs.mkdirSync(userDir, { recursive: true });
    fs.writeFileSync(path.join(userDir, 'claude.json'), '{ not json');
    const logs: any[] = [];
    const reg = createRegistry({ bundledDir, userDir, cacheDir, log: (m: any) => logs.push(m) });
    expect(reg.get('claude').version).toBe('bundled-1');
    expect(logs.some(l => String(l).includes('claude'))).toBe(true);
  });

  it('skips a schema-invalid override and falls back', () => {
    write(userDir, 'claude', { agent: 'claude', version: 'bad', minEngineVersion: 1, rules: [] });
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('claude').version).toBe('bundled-1');
    const report = reg.reload();
    expect(report.skipped.some((s: any) => s.agent === 'claude' && s.source === 'user')).toBe(true);
  });

  it('returns null for an unknown agent', () => {
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('nonesuch')).toBeNull();
    expect(reg.sourceOf('nonesuch')).toBeNull();
  });

  it('tolerates missing user and cache directories', () => {
    const reg = createRegistry({ bundledDir, userDir: path.join(root, 'nope'), cacheDir: path.join(root, 'nada') });
    expect(reg.get('claude').version).toBe('bundled-1');
  });

  it('reload picks up a newly written override', () => {
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('claude').version).toBe('bundled-1');
    write(userDir, 'claude', manifestDoc('claude', 'user-9'));
    const report = reg.reload();
    expect(report.loaded).toContain('claude');
    expect(reg.get('claude').version).toBe('user-9');
  });

  it('list reports every agent with its version and source', () => {
    write(userDir, 'codex', manifestDoc('codex', 'user-1'));
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    const list = reg.list().sort((a: any, b: any) => a.agent.localeCompare(b.agent));
    expect(list).toEqual([
      { agent: 'claude', version: 'bundled-1', source: 'bundled' },
      { agent: 'codex', version: 'user-1', source: 'user' },
    ]);
  });

  it('normalizes case across sources so an override with different casing does not mint a phantom agent', () => {
    write(userDir, 'CLAUDE', manifestDoc('claude', 'user-upper'));
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    const list = reg.list();
    expect(list).toEqual([{ agent: 'claude', version: 'user-upper', source: 'user' }]);
  });

  it('resolves get/sourceOf regardless of the casing passed in', () => {
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('CLAUDE')).toEqual(reg.get('claude'));
    expect(reg.sourceOf('CLAUDE')).toBe('bundled');
  });

  it('skips a source whose directory somehow contains colliding case-variant filenames for one agent', () => {
    const realReaddir = fs.readdirSync.bind(fs);
    const spy = vi.spyOn(fs, 'readdirSync').mockImplementation(((dir: any, opts?: any) => {
      if (dir === userDir) return ['claude.json', 'CLAUDE.json'] as any;
      return realReaddir(dir, opts);
    }) as any);
    try {
      const logs: any[] = [];
      const reg = createRegistry({ bundledDir, userDir, cacheDir, log: (m: any) => logs.push(m) });
      // falls back past the colliding source to the next one (bundled)
      expect(reg.get('claude').version).toBe('bundled-1');
      const report = reg.reload();
      const collision = report.skipped.find((s: any) => s.agent === 'claude' && s.source === 'user');
      expect(collision).toBeDefined();
      expect(collision.error).toMatch(/collid/i);
      expect(collision.error).toContain('claude.json');
      expect(collision.error).toContain('CLAUDE.json');
      expect(logs.some(l => String(l).includes('claude'))).toBe(true);
    } finally {
      spy.mockRestore();
    }
  });

  it('strips a leading UTF-8 BOM before parsing a manifest', () => {
    fs.mkdirSync(userDir, { recursive: true });
    const json = JSON.stringify(manifestDoc('claude', 'user-bom'));
    fs.writeFileSync(path.join(userDir, 'claude.json'), '﻿' + json);
    const reg = createRegistry({ bundledDir, userDir, cacheDir });
    expect(reg.get('claude').version).toBe('user-bom');
  });
});
