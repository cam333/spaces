// tests/proto/fs-folders.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { listFolders, readDevRoots, createRuntime } from '../../bin/lib/runtime/index.js';

// The terminal client's directory browser. Confined to the dev directories,
// like the web app's folder picker.

let base: string, dev: string, outside: string;

beforeEach(() => {
  base = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-folders-')));
  dev = path.join(base, 'dev');
  outside = path.join(base, 'secret');
  for (const d of ['dev/beta', 'dev/alpha/inner', 'dev/.hidden', 'dev/node_modules', 'secret']) {
    fs.mkdirSync(path.join(base, d), { recursive: true });
  }
  fs.writeFileSync(path.join(dev, 'file.txt'), 'x');
});

afterEach(() => fs.rmSync(base, { recursive: true, force: true }));

const names = (r: any) => r.result.folders.map((f: any) => f.name);

describe('listFolders', () => {
  it('lists the first root by default: folders only, sorted, hidden and node_modules left out', () => {
    const r = listFolders(undefined, [dev]) as any;
    expect(r.ok).toBe(true);
    expect(r.result.current).toBe(dev);
    expect(r.result.parent).toBeNull();
    expect(names(r)).toEqual(['alpha', 'beta']);
    expect(r.result.folders[0].path).toBe(path.join(dev, 'alpha'));
  });

  it('gives a subfolder a parent to go back up to', () => {
    const r = listFolders(path.join(dev, 'alpha'), [dev]) as any;
    expect(r.result.parent).toBe(dev);
    expect(names(r)).toEqual(['inner']);
  });

  it('refuses a folder outside the dev directories, including via ..', () => {
    expect((listFolders(outside, [dev]) as any).code).toBe('bad_request');
    expect((listFolders(path.join(dev, '..'), [dev]) as any).code).toBe('bad_request');
    // A sibling whose name merely starts with the root's is outside too.
    fs.mkdirSync(dev + '-evil');
    expect((listFolders(dev + '-evil', [dev]) as any).code).toBe('bad_request');
  });

  it('does not follow a symlink out of the dev directories', () => {
    try {
      fs.symlinkSync(outside, path.join(dev, 'escape'), 'junction');
    } catch {
      return; // no symlink permission on this machine
    }
    expect(names(listFolders(dev, [dev]))).not.toContain('escape');
    expect((listFolders(path.join(dev, 'escape'), [dev]) as any).code).toBe('bad_request');
  });

  it('reports a missing folder as not_found', () => {
    expect((listFolders(path.join(dev, 'nope'), [dev]) as any).code).toBe('not_found');
  });
});

describe('readDevRoots', () => {
  it('uses configured dev directories, else the home directory', () => {
    const spacesDir = path.join(base, '.spaces');
    fs.mkdirSync(spacesDir);
    expect(readDevRoots(spacesDir, dev)).toEqual([dev]);
    fs.writeFileSync(path.join(spacesDir, 'config.json'), JSON.stringify({ devDirectories: [outside, path.join(base, 'gone')] }));
    expect(readDevRoots(spacesDir, dev)).toEqual([outside]);
  });
});

describe('fs.folders over dispatch', () => {
  it('answers from the runtime, confined to its config', () => {
    const spacesDir = path.join(base, '.spaces');
    fs.mkdirSync(spacesDir);
    fs.writeFileSync(path.join(spacesDir, 'config.json'), JSON.stringify({ devDirectories: [dev] }));
    process.env.SPACES_NO_DETECTION_UPDATE = '1';
    const rt = createRuntime({ nodeId: 'test', spacesDbPath: path.join(spacesDir, 'spaces.db') });
    try {
      expect(names(rt.dispatch('fs.folders', {}))).toEqual(['alpha', 'beta']);
      expect((rt.dispatch('fs.folders', { path: outside }) as any).error.code).toBe('bad_request');
      expect((rt.dispatch('fs.folders', { path: 42 }) as any).error.code).toBe('bad_request');
    } finally {
      try { rt.sessionStore.close(); } catch { /* */ }
    }
  });
});
