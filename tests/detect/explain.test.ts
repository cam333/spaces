import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRuntime } from '../../bin/lib/runtime/index.js';

let home: string;
let prevHome: string | undefined, prevProfile: string | undefined;

beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-explain-'));
  prevHome = process.env.HOME; prevProfile = process.env.USERPROFILE;
  process.env.HOME = home; process.env.USERPROFILE = home;
});

afterEach(() => {
  if (prevHome === undefined) delete process.env.HOME; else process.env.HOME = prevHome;
  if (prevProfile === undefined) delete process.env.USERPROFILE; else process.env.USERPROFILE = prevProfile;
  fs.rmSync(home, { recursive: true, force: true });
});

function runtimeWithPane(agentType = 'claude') {
  const rt = createRuntime({ nodeId: 'test', spacesDbPath: '' });
  rt.registerPane('p1', {
    cols: 80, rows: 24, agentType, cwd: '.', workspaceId: null,
    onInput: () => {}, onResize: () => {},
  });
  return rt;
}

const settle = () => new Promise(r => setTimeout(r, 60));

describe('detect protocol methods', () => {
  it('detect.explain reports state, manifest and evidence for a live pane', async () => {
    const rt = runtimeWithPane('claude');
    rt.feed('p1', 'Do you want to proceed?\r\n❯ 1. Yes\r\n');
    await settle();
    rt.detect.tick();
    const res = rt.dispatch('detect.explain', { paneId: 'p1' }) as any;
    expect(res.error).toBeUndefined();
    expect(res.result.paneId).toBe('p1');
    expect(res.result.agentType).toBe('claude');
    expect(res.result.manifest.agent).toBe('claude');
    expect(res.result.manifest.source).toBe('bundled');
    expect(Array.isArray(res.result.evidence)).toBe(true);
    expect(res.result.evidence.length).toBeGreaterThan(0);
    expect(res.result.detected.state).toBe('blocked');
  });

  it('re-marks the pane dirty from the async parse callback, so a tick that lands before parsing completes still gets caught', async () => {
    const rt = runtimeWithPane('claude');
    rt.feed('p1', 'Do you want to proceed?\r\n❯ 1. Yes\r\n');
    // Simulate a 300ms tick landing inside xterm's async write-buffer
    // window: evaluate (and clear dirty) before the parse callback fires.
    rt.detect.tick();
    const first = rt.detect.lastResult('p1');
    expect(first).not.toBeNull();

    // No further feed() call — only the write callback (fired once parsing
    // actually completes) can re-dirty the pane from here.
    await settle();
    rt.detect.tick();
    const second = rt.detect.lastResult('p1');
    expect(second!.at).toBeGreaterThan(first!.at);
  });

  it('detect.explain returns not_found for an unknown pane', () => {
    const rt = runtimeWithPane();
    const res = rt.dispatch('detect.explain', { paneId: 'nope' }) as any;
    expect(res.error.code).toBe('not_found');
  });

  it('detect.explain rejects a missing paneId', () => {
    const rt = runtimeWithPane();
    expect((rt.dispatch('detect.explain', {}) as any).error.code).toBe('bad_request');
    expect((rt.dispatch('detect.explain', { paneId: '' }) as any).error.code).toBe('bad_request');
  });

  it('detect.manifests lists the bundled agents with the engine version', () => {
    const rt = runtimeWithPane();
    const res = rt.dispatch('detect.manifests', {}) as any;
    expect(res.result.engineVersion).toBe(1);
    const agents = res.result.manifests.map((m: any) => m.agent).sort();
    expect(agents).toEqual(['aider', 'claude', 'codex', 'gemini']);
    expect(res.result.manifests.every((m: any) => m.source === 'bundled')).toBe(true);
  });

  it('detect.reload picks up a user override written after startup', () => {
    const rt = runtimeWithPane();
    const dir = path.join(home, '.spaces', 'detection');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'claude.json'), JSON.stringify({
      agent: 'claude', version: 'user-override', minEngineVersion: 1,
      rules: [{ id: 'r', state: 'idle', priority: 1, region: 'whole_visible', contains: [''] }],
    }));
    const res = rt.dispatch('detect.reload', {}) as any;
    expect(res.result.loaded).toContain('claude');
    const listed = (rt.dispatch('detect.manifests', {}) as any).result.manifests
      .find((m: any) => m.agent === 'claude');
    expect(listed.version).toBe('user-override');
    expect(listed.source).toBe('user');
  });

  it('detect.reload reports a malformed override as skipped', () => {
    const rt = runtimeWithPane();
    const dir = path.join(home, '.spaces', 'detection');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'codex.json'), '{ broken');
    const res = rt.dispatch('detect.reload', {}) as any;
    expect(res.result.skipped.some((s: any) => s.agent === 'codex')).toBe(true);
    const listed = (rt.dispatch('detect.manifests', {}) as any).result.manifests
      .find((m: any) => m.agent === 'codex');
    expect(listed.source).toBe('bundled'); // fell back, still usable
  });
});
