import { describe, it, expect } from 'vitest';
import fs from 'fs';
import path from 'path';

const SRC = fs.readFileSync(
  path.resolve(__dirname, '../../src/lib/network/client.ts'),
  'utf-8',
);

describe('NetworkClient TLS handling', () => {
  it('never references the process-global NODE_TLS_REJECT_UNAUTHORIZED', () => {
    // The whole point of the fix: federation must NOT disable TLS verification
    // process-wide. Any reference to the global kill-switch is a regression.
    expect(SRC).not.toContain('NODE_TLS_REJECT_UNAUTHORIZED');
  });

  it('only relaxes TLS per-request, behind an explicit opt-in flag', () => {
    // If the client ever disables cert verification, it must be (a) scoped to a
    // single request via a dispatcher/agent, and (b) gated behind an explicit
    // operator opt-in env var — never on by default, never process-global.
    if (SRC.includes('rejectUnauthorized: false')) {
      expect(SRC).toContain('dispatcher');
      expect(SRC).toMatch(/SPACES_SKIP_TLS_VERIFY|SPACES_FEDERATION_ALLOW_SELF_SIGNED/);
    }
  });
});
