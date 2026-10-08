import { describe, it, expect } from 'vitest';
import path from 'node:path';
import { resolveAgentGuide } from '../../bin/lib/agent-guide.js';

const REPO_ROOT = path.join(__dirname, '../..');

describe('resolveAgentGuide', () => {
  it('finds the guide in a checkout (public/agent-guide.md)', () => {
    const p = resolveAgentGuide(REPO_ROOT);
    expect(p).not.toBeNull();
    expect(p).toMatch(/agent-guide\.md$/);
  });

  it('returns null when no guide exists under the root', () => {
    expect(resolveAgentGuide(path.join(REPO_ROOT, 'certs'))).toBeNull();
  });
});
