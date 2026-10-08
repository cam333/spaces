// bin/lib/runtime/git-status.js
'use strict';

const { execFile } = require('child_process');

/**
 * The branch, ahead/behind counts and dirtiness of a space's working copy,
 * for the sidebar's second line.
 *
 * A space has no directory of its own; its panes do. The space's directory
 * is its first pane's `cwd` — the one it was created around — which is the
 * same rule the sidebar uses to describe it.
 *
 * One `git status --porcelain=v2 --branch` answers everything, and it runs
 * off the request path: snapshots read a cache, and a background refresh
 * emits `workspace.updated` when a space's status actually changes.
 */

const REFRESH_MS = 5000;
const GIT_TIMEOUT_MS = 2000;

/**
 * Parses `git status --porcelain=v2 --branch` output. `null` when it is not
 * a repository's status at all.
 */
function parsePorcelain(text) {
  let branch = null;
  let ahead = 0;
  let behind = 0;
  let dirty = false;
  let sawHeader = false;
  for (const line of text.split('\n')) {
    if (line.startsWith('# branch.head ')) {
      sawHeader = true;
      const head = line.slice('# branch.head '.length).trim();
      branch = head === '(detached)' ? null : head;
    } else if (line.startsWith('# branch.oid ')) {
      sawHeader = true;
    } else if (line.startsWith('# branch.ab ')) {
      const m = /\+(\d+) -(\d+)/.exec(line);
      if (m) { ahead = Number(m[1]); behind = Number(m[2]); }
    } else if (line && !line.startsWith('#')) {
      dirty = true;
    }
  }
  if (!sawHeader) return null;
  return { branch, ahead, behind, dirty };
}

/** Runs git in `cwd`; resolves to its parsed status or null. Never rejects. */
function readGitStatus(cwd, run = execFile) {
  return new Promise((resolve) => {
    if (!cwd) return resolve(null);
    run(
      'git',
      ['-C', cwd, 'status', '--porcelain=v2', '--branch', '--untracked-files=normal'],
      { timeout: GIT_TIMEOUT_MS, windowsHide: true, maxBuffer: 4 * 1024 * 1024 },
      (error, stdout) => resolve(error ? null : parsePorcelain(String(stdout))),
    );
  });
}

const same = (a, b) =>
  a === b || (!!a && !!b && a.branch === b.branch && a.ahead === b.ahead
    && a.behind === b.behind && a.dirty === b.dirty);

/**
 * Keeps a per-space cache fresh. `spaceDirs()` returns `[workspaceId, cwd]`
 * pairs; `onChange(workspaceId, git)` fires when a space's status differs
 * from what was last reported.
 */
function createGitTracker({ spaceDirs, onChange, read = readGitStatus, intervalMs = REFRESH_MS }) {
  const cache = new Map(); // workspaceId -> status | null
  let running = false;

  async function refresh() {
    if (running) return;
    running = true;
    try {
      const pairs = spaceDirs();
      const seen = new Set();
      // Directories are shared between spaces often enough to ask git once each.
      const byDir = new Map();
      for (const [, cwd] of pairs) if (cwd && !byDir.has(cwd)) byDir.set(cwd, read(cwd));
      for (const [workspaceId, cwd] of pairs) {
        seen.add(workspaceId);
        const status = cwd ? await byDir.get(cwd) : null;
        const had = cache.has(workspaceId);
        if (had && same(cache.get(workspaceId), status)) continue;
        cache.set(workspaceId, status);
        // A space that was never a repository has nothing new to say.
        if (had || status) onChange(workspaceId, status);
      }
      for (const id of [...cache.keys()]) if (!seen.has(id)) cache.delete(id);
    } finally {
      running = false;
    }
  }

  const timer = setInterval(() => { refresh().catch(() => {}); }, intervalMs);
  if (timer.unref) timer.unref();

  return {
    get: (workspaceId) => cache.get(workspaceId) ?? null,
    refresh,
    stop: () => clearInterval(timer),
  };
}

module.exports = { parsePorcelain, readGitStatus, createGitTracker, REFRESH_MS };
