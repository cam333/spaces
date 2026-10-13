#!/usr/bin/env node
'use strict';

/**
 * Writes the agent-detection rules for agentspaces.co: `detection/index.json`
 * and one manifest per agent, copied from the ones bundled in this package
 * (bin/lib/runtime/detect/manifests). Installs poll the index daily and pick
 * up a manifest whose version differs from the one they last fetched, so a
 * rule fix reaches everyone without a release.
 *
 *   node scripts/publish-detection.js <site checkout>
 *
 * Then commit and push the site. Each manifest is compiled first: one that
 * this build's engine rejects is not published.
 */

const fs = require('fs');
const path = require('path');
const { compileManifest, ENGINE_VERSION } = require('../bin/lib/runtime/detect/manifest');

const SITE_URL = 'https://www.agentspaces.co';
const site = process.argv[2];
if (!site || !fs.existsSync(site)) {
  console.error('usage: node scripts/publish-detection.js <site checkout>');
  process.exit(2);
}

const source = path.join(__dirname, '..', 'bin', 'lib', 'runtime', 'detect', 'manifests');
const out = path.join(site, 'detection');
fs.mkdirSync(out, { recursive: true });

const manifests = [];
for (const file of fs.readdirSync(source).filter((f) => f.endsWith('.json')).sort()) {
  const doc = JSON.parse(fs.readFileSync(path.join(source, file), 'utf-8'));
  const compiled = compileManifest(doc);
  if (!compiled.ok) {
    console.error(`${file}: ${compiled.error}`);
    process.exit(1);
  }
  fs.writeFileSync(path.join(out, `${doc.agent}.json`), `${JSON.stringify(doc, null, 2)}\n`);
  manifests.push({ agent: doc.agent, version: doc.version, url: `${SITE_URL}/detection/${doc.agent}.json` });
}

const index = { engineVersion: ENGINE_VERSION, manifests };
fs.writeFileSync(path.join(out, 'index.json'), `${JSON.stringify(index, null, 2)}\n`);
for (const m of manifests) console.log(`${m.agent} ${m.version}`);
console.log(`wrote ${path.join(out, 'index.json')}`);
