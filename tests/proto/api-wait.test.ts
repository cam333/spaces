// tests/proto/api-wait.test.ts
import { describe, it, expect, vi, afterEach } from 'vitest';
import { EventEmitter } from 'node:events';
import { waitForApi } from '../../bin/terminal-server.js';

// The server probes its own API until it answers. A probe that times out is
// destroyed, and a destroyed request also reports an error, as Node's does.
// Each probe must lead to exactly one more; two per round doubled them until
// a million ran at once and the server ran out of memory.
function fakeGet(calls: EventEmitter[]) {
  return (_url: string, _opts: unknown, _onResponse: unknown) => {
    const req = Object.assign(new EventEmitter(), {
      destroy() { setImmediate(() => req.emit('error', new Error('socket hang up'))); },
    });
    calls.push(req);
    return req;
  };
}

describe('waitForApi', () => {
  afterEach(() => vi.useRealTimers());

  it('sends one probe per round when probes time out', async () => {
    vi.useFakeTimers();
    const calls: EventEmitter[] = [];
    waitForApi(fakeGet(calls) as any);
    for (let round = 0; round < 12; round++) {
      await vi.advanceTimersByTimeAsync(2000);
      // Time the newest probe out, as the real one would at 1 s.
      calls[calls.length - 1].emit('timeout');
      await vi.advanceTimersByTimeAsync(0);
    }
    await vi.advanceTimersByTimeAsync(2000);
    expect(calls.length).toBeLessThanOrEqual(14);
  });

  it('sends one probe per round when probes fail outright', async () => {
    vi.useFakeTimers();
    const calls: EventEmitter[] = [];
    waitForApi(fakeGet(calls) as any);
    for (let round = 0; round < 5; round++) {
      await vi.advanceTimersByTimeAsync(2000);
      calls[calls.length - 1].emit('error', new Error('ECONNREFUSED'));
    }
    await vi.advanceTimersByTimeAsync(2000);
    expect(calls.length).toBe(6);
  });
});
