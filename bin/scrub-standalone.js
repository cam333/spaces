#!/usr/bin/env node
'use strict';
// Remove runtime-generated files from .next/standalone/ before publishing to npm.
// Agent hook logs and per-user state can land in the build output at runtime;
// this scrub runs on every publish so none of it is ever packed into the tarball.

const fs = require('fs');
const path = require('path');

const STANDALONE = path.join(process.cwd(), '.next', 'standalone');

const DENY_FILES = [];

const DENY_DIRS = [
  '.spaces',
  '.claude',
  '.codex',
  '.gemini',
  '.pi',
  'logs',
  // Internal planning/design docs — embed maintainer-local paths and were
  // never meant to ship to end users. Keeping them at HEAD (for collaboration)
  // but not in the published tarball.
  'plans',
  'superpowers',
];

const DENY_PATTERNS = [
  /\.log$/i,
  /^\.env(\..+)?$/i,
  /^\.npmrc$/i,
  // Server source maps: nothing reads them at runtime.
  /\.js\.map$/i,
];

function walk(dir, cb) {
  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }); }
  catch { return; }
  for (const e of entries) {
    const full = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (DENY_DIRS.includes(e.name)) {
        fs.rmSync(full, { recursive: true, force: true });
        console.log('[scrub] removed dir: ' + path.relative(STANDALONE, full));
        continue;
      }
      walk(full, cb);
    } else if (e.isFile()) {
      cb(full, e.name);
    }
  }
}

if (!fs.existsSync(STANDALONE)) {
  console.log('[scrub] no .next/standalone to scrub');
  process.exit(0);
}

// Project files the standalone trace can sweep in (see
// outputFileTracingExcludes in next.config.ts, which should already keep
// them out). Only ever matched at the top of .next/standalone, never inside
// node_modules or .next. None of them is read at runtime; a local tui/target
// alone can be gigabytes.
const PROJECT_DIRS = ['tests', 'docs', 'tui', 'scripts', 'npm', 'src', 'bin', 'coverage', '.github'];
const PROJECT_FILES = [
  'nginx.conf',
  'docker-compose.yml',
  'package-lock.json',
  'tsconfig.tsbuildinfo',
  'tsconfig.json',
  'vitest.config.ts',
  'eslint.config.mjs',
  'postcss.config.mjs',
  'next.config.ts',
  'rust-toolchain.toml',
];
const PROJECT_PATTERNS = [/\.md$/i, /\.gif$/i];

for (const name of PROJECT_DIRS) {
  const full = path.join(STANDALONE, name);
  if (fs.existsSync(full)) {
    fs.rmSync(full, { recursive: true, force: true });
    console.log('[scrub] removed project dir: ' + name);
  }
}
for (const e of fs.readdirSync(STANDALONE, { withFileTypes: true })) {
  if (!e.isFile()) continue;
  if (PROJECT_FILES.includes(e.name) || PROJECT_PATTERNS.some((p) => p.test(e.name))) {
    fs.rmSync(path.join(STANDALONE, e.name), { force: true });
    console.log('[scrub] removed project file: ' + e.name);
  }
}
// Demo recordings for the README; the web app never serves them.
const PUBLIC = path.join(STANDALONE, 'public');
if (fs.existsSync(PUBLIC)) {
  for (const name of fs.readdirSync(PUBLIC)) {
    if (/\.gif$/i.test(name)) {
      fs.rmSync(path.join(PUBLIC, name), { force: true });
      console.log('[scrub] removed public/' + name);
    }
  }
}

walk(STANDALONE, (full, name) => {
  if (DENY_FILES.includes(name) || DENY_PATTERNS.some(p => p.test(name))) {
    fs.rmSync(full, { force: true });
    console.log('[scrub] removed file: ' + path.relative(STANDALONE, full));
  }
});

// Final scan: refuse to publish if any obvious secrets remain in text files
const SECRET_PATTERNS = [
  /npm_[A-Za-z0-9]{30,}/,
  /sk-ant-[A-Za-z0-9_-]{20,}/,
  /ghp_[A-Za-z0-9]{30,}/,
];
let leaked = false;
walk(STANDALONE, (full, name) => {
  if (!/\.(js|json|md|txt|log|ts|tsx|mjs|cjs)$/i.test(name)) return;
  let content;
  try { content = fs.readFileSync(full, 'utf-8'); } catch { return; }
  for (const p of SECRET_PATTERNS) {
    const m = content.match(p);
    if (m) {
      console.error('[scrub] SECRET DETECTED in ' + path.relative(STANDALONE, full) + ': ' + m[0].slice(0, 12) + '...');
      leaked = true;
    }
  }
});

if (leaked) {
  console.error('[scrub] aborting publish — secrets found in build output');
  process.exit(1);
}

console.log('[scrub] done — standalone clean');
