// tests/runtime/git-status.test.ts
import { describe, it, expect } from 'vitest';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { parsePorcelain, readGitStatus, createGitTracker } from '../../bin/lib/runtime/git-status.js';

describe('parsePorcelain', () => {
  it('reads branch, ahead/behind and a clean tree', () => {
    expect(parsePorcelain('# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -1\n'))
      .toEqual({ branch: 'main', ahead: 2, behind: 1, dirty: false });
  });

  it('counts any entry line as dirty', () => {
    expect(parsePorcelain('# branch.oid abc\n# branch.head dev\n1 .M N... 100644 100644 100644 a b file.txt\n'))
      .toEqual({ branch: 'dev', ahead: 0, behind: 0, dirty: true });
    expect(parsePorcelain('# branch.oid abc\n# branch.head dev\n? new.txt\n')!.dirty).toBe(true);
  });

  it('has no branch when HEAD is detached', () => {
    expect(parsePorcelain('# branch.oid abc\n# branch.head (detached)\n')!.branch).toBeNull();
  });

  it('is null for output that is not a status', () => {
    expect(parsePorcelain('')).toBeNull();
    expect(parsePorcelain('fatal: not a git repository\n')).toBeNull();
  });
});

describe('readGitStatus', () => {
  it('reads a real repository and is null outside one', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-git-'));
    try {
      expect(await readGitStatus(dir)).toBeNull();
      execFileSync('git', ['init', '-q', '-b', 'trunk'], { cwd: dir });
      fs.writeFileSync(path.join(dir, 'a.txt'), 'x');
      expect(await readGitStatus(dir)).toEqual({ branch: 'trunk', ahead: 0, behind: 0, dirty: true });
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe('createGitTracker', () => {
  const statusOf = (branch: string) => ({ branch, ahead: 0, behind: 0, dirty: false });

  it('reports a space once, then only when its status changes', async () => {
    let current: any = statusOf('main');
    const changes: any[] = [];
    const tracker = createGitTracker({
      spaceDirs: () => [['w1', '/repo']],
      onChange: (id: string, git: any) => changes.push([id, git]),
      read: async () => current,
    });
    try {
      await tracker.refresh();
      await tracker.refresh();
      current = statusOf('feature');
      await tracker.refresh();
      expect(changes).toEqual([['w1', statusOf('main')], ['w1', statusOf('feature')]]);
      expect(tracker.get('w1')).toEqual(statusOf('feature'));
    } finally {
      tracker.stop();
    }
  });

  it('says nothing about a space that was never a repository', async () => {
    const changes: any[] = [];
    const tracker = createGitTracker({
      spaceDirs: () => [['w1', '/tmp']],
      onChange: (id: string, git: any) => changes.push([id, git]),
      read: async () => null,
    });
    try {
      await tracker.refresh();
      expect(changes).toEqual([]);
      expect(tracker.get('w1')).toBeNull();
    } finally {
      tracker.stop();
    }
  });

  it('asks git once per directory shared by several spaces', async () => {
    let calls = 0;
    const tracker = createGitTracker({
      spaceDirs: () => [['w1', '/repo'], ['w2', '/repo']],
      onChange: () => {},
      read: async () => { calls++; return statusOf('main'); },
    });
    try {
      await tracker.refresh();
      expect(calls).toBe(1);
    } finally {
      tracker.stop();
    }
  });
});

describe('a space\'s git status in the runtime', () => {
  it('appears in the snapshot and arrives as workspace.updated', async () => {
    const { createRuntime } = await import('../../bin/lib/runtime/index.js');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-gitrt-'));
    process.env.SPACES_NO_DETECTION_UPDATE = '1';
    const runtime: any = createRuntime({ nodeId: 'test', trackGit: true });
    try {
      execFileSync('git', ['init', '-q', '-b', 'trunk'], { cwd: dir });
      const events: any[] = [];
      runtime.hub.subscribe((e: any) => events.push([e.type, e.payload]));
      runtime.state.open('p1', { workspaceId: 'w1', agentType: 'shell', cwd: dir });
      await runtime.gitTracker.refresh();
      const git = { branch: 'trunk', ahead: 0, behind: 0, dirty: false };
      expect(events).toContainEqual(['workspace.updated', { workspaceId: 'w1', git }]);
      const ws = runtime.dispatch('session.snapshot', {}).result.workspaces.find((w: any) => w.id === 'w1');
      expect(ws.git).toEqual(git);
    } finally {
      runtime.gitTracker.stop();
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
