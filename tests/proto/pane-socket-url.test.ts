import { describe, it, expect } from 'vitest';
import { buildPaneSocketUrl, encodeUpFrame } from '../../src/lib/proto/pane-socket';

describe('pane socket helpers', () => {
  it('builds ws url with mode and token', () => {
    expect(buildPaneSocketUrl({
      origin: 'http://localhost:3457', basePath: '', paneId: 'p 1', mode: 'observe', token: 'tok',
    })).toBe('ws://localhost:3457/proto/pane/p%201?mode=observe&terminalToken=tok');
  });

  it('builds wss url for https origins and control mode with takeover', () => {
    expect(buildPaneSocketUrl({
      origin: 'https://spaces.example', basePath: '/app', paneId: 'p1', mode: 'control', takeover: true, token: 't',
    })).toBe('wss://spaces.example/app/proto/pane/p1?mode=control&takeover=1&terminalToken=t');
  });

  it('encodes input and resize up-frames', () => {
    expect(JSON.parse(encodeUpFrame({ t: 'i', data: 'ls\r' }))).toEqual({ t: 'i', data: 'ls\r' });
    expect(JSON.parse(encodeUpFrame({ t: 'r', cols: 80, rows: 24 }))).toEqual({ t: 'r', cols: 80, rows: 24 });
  });
});
