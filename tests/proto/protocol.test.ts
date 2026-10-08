import { describe, it, expect } from 'vitest';
import { PROTOCOL_VERSION, ERROR_CODES, schemas } from '../../bin/lib/runtime/protocol.js';

describe('protocol schemas', () => {
  it('exports version 1 and the error code set', () => {
    expect(PROTOCOL_VERSION).toBe(1);
    expect(ERROR_CODES).toContain('version_mismatch');
    expect(ERROR_CODES).toContain('gap');
  });

  it('validates a hello handshake', () => {
    expect(schemas.Hello.safeParse({
      type: 'hello', protocolVersion: 1, client: 'web',
    }).success).toBe(true);
    expect(schemas.Hello.safeParse({ type: 'hello' }).success).toBe(false);
  });

  it('validates request/response envelopes', () => {
    expect(schemas.Request.safeParse({
      id: 'r1', method: 'session.snapshot', params: {},
    }).success).toBe(true);
    expect(schemas.OkResponse.safeParse({ id: 'r1', result: { any: 'thing' } }).success).toBe(true);
    expect(schemas.ErrResponse.safeParse({
      id: 'r1', error: { code: 'not_found', message: 'no such pane' },
    }).success).toBe(true);
    expect(schemas.ErrResponse.safeParse({
      id: 'r1', error: { code: 'nope', message: 'x' },
    }).success).toBe(false);
  });

  it('validates a snapshot with panes and workspaces', () => {
    const snap = {
      protocolVersion: 1, nodeId: 'local', seq: 42,
      workspaces: [{ id: 'w1', name: 'frontend', color: '#89b4fa', paneIds: ['p1'] }],
      panes: [{
        id: 'p1', workspaceId: 'w1', agentType: 'claude',
        agentState: 'working', seen: true, title: 'claude', cwd: 'C:\\work',
        sessionRef: null, live: true, color: '#ef4444',
      }],
    };
    const parsed = schemas.Snapshot.safeParse(snap);
    expect(parsed.success).toBe(true);
    expect(schemas.Snapshot.safeParse({ ...snap, panes: [{ ...snap.panes[0], agentState: 'busy' }] }).success).toBe(false);
  });

  it('validates pane up-frames (input and resize only)', () => {
    expect(schemas.PaneUpFrame.safeParse({ t: 'i', data: 'ls\r' }).success).toBe(true);
    expect(schemas.PaneUpFrame.safeParse({ t: 'r', cols: 120, rows: 30 }).success).toBe(true);
    expect(schemas.PaneUpFrame.safeParse({ t: 'x' }).success).toBe(false);
  });
});
