'use strict';

const path = require('path');
const fs = require('fs');

/**
 * Where the agent guide lives relative to the package root, in resolution
 * order: a development checkout serves it straight from `public/`; an
 * installed package only ships `.next/standalone/`, into which the build
 * copies `public/`.
 */
const CANDIDATES = [
  ['public', 'agent-guide.md'],
  ['.next', 'standalone', 'public', 'agent-guide.md'],
];

/** Absolute path to the bundled agent guide under `root`, or null. */
function resolveAgentGuide(root) {
  for (const parts of CANDIDATES) {
    const candidate = path.join(root, ...parts);
    try {
      if (fs.existsSync(candidate)) return candidate;
    } catch {
      // An unreadable path is simply not a candidate.
    }
  }
  return null;
}

module.exports = { resolveAgentGuide };
