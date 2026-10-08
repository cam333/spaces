// Desktop mode answers its owner and nobody else — driven through the real
// upgrade handler in bin/terminal-server.js. See bin/lib/desktop-auth.js.
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import http from 'node:http';
import { WebSocket } from 'ws';
import { signDesktop } from '../../bin/lib/desktop-auth.js';

const SECRET = '9'.repeat(64);
process.env.SPACES_INTERNAL_TOKEN = SECRET;
process.env.SPACES_NO_DETECTION_UPDATE = '1';

let server: http.Server;
let port: number;

beforeAll(async () => {
  const { createTerminalServer } = await import('../../bin/terminal-server.js');
  server = http.createServer((_req, res) => { res.statusCode = 404; res.end(); });
  createTerminalServer(server);
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', () => r()));
  port = (server.address() as any).port;
});

afterAll(() => new Promise<void>((r) => server.close(() => r())));

/** 'ok' for hello-ok, the error code for a refusal, 'rejected' if the upgrade never opened. */
function hello(path: string, headers: Record<string, string> = {}): Promise<string> {
  return new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}${path}`, { headers });
    const timer = setTimeout(() => { ws.terminate(); resolve('timeout'); }, 4000);
    const finish = (v: string) => { clearTimeout(timer); try { ws.terminate(); } catch {} resolve(v); };
    ws.once('open', () => ws.send(JSON.stringify({ type: 'hello', protocolVersion: 1, client: 'owner-test' })));
    ws.once('message', (d) => {
      const m = JSON.parse(d.toString());
      finish(m.type === 'hello-ok' ? 'ok' : (m.error?.code ?? 'other'));
    });
    ws.once('error', () => finish('rejected'));
    ws.once('unexpected-response', () => finish('rejected'));
  });
}

describe('the control channel (/proto)', () => {
  it('refuses a connection with no credentials, loopback or not', async () => {
    expect(await hello('/proto')).toBe('unauthorized');
  });

  it('refuses the old magic token and a query-string secret', async () => {
    expect(await hello('/proto?terminalToken=desktop-local')).toBe('unauthorized');
    expect(await hello(`/proto?terminalToken=${SECRET}`)).toBe('unauthorized');
  });

  it('refuses a wrong secret and a session signed with another', async () => {
    expect(await hello('/proto', { Authorization: `Bearer ${'8'.repeat(64)}` })).toBe('unauthorized');
    const foreign = signDesktop('7'.repeat(64), 'session', 60);
    expect(await hello('/proto', { Authorization: `Bearer ${foreign}` })).toBe('unauthorized');
  });

  it('refuses a login code presented as a session', async () => {
    expect(await hello('/proto', { Authorization: `Bearer ${signDesktop(SECRET, 'login', 60)}` })).toBe('unauthorized');
  });

  it('accepts the secret, a session token, and the browser cookie', async () => {
    expect(await hello('/proto', { Authorization: `Bearer ${SECRET}` })).toBe('ok');
    expect(await hello('/proto', { 'x-spaces-internal': SECRET })).toBe('ok');
    expect(await hello('/proto', { Authorization: `Bearer ${signDesktop(SECRET, 'session', 60)}` })).toBe('ok');
    expect(await hello('/proto', { Cookie: `spaces-desktop=${signDesktop(SECRET, 'session', 60)}` })).toBe('ok');
  });

  it('refuses a rebound hostname even with valid credentials', async () => {
    const evil = `attacker.example:${port}`;
    expect(await hello('/proto', {
      Host: evil, Origin: `http://${evil}`, Authorization: `Bearer ${SECRET}`,
    })).toBe('rejected');
  });
});

describe('browser terminals (/ws)', () => {
  /** True when the server spawned a pane for this connection. */
  function spawned(query: string, headers: Record<string, string> = {}): Promise<boolean> {
    return new Promise((resolve) => {
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws?paneId=owner-${Math.random().toString(36).slice(2)}&${query}`, { headers });
      const timer = setTimeout(() => { ws.terminate(); resolve(false); }, 6000);
      const finish = (v: boolean) => { clearTimeout(timer); try { ws.terminate(); } catch {} resolve(v); };
      ws.on('message', (d) => {
        try { if (JSON.parse(d.toString()).type === 'ready') finish(true); } catch {}
      });
      ws.once('close', () => finish(false));
      ws.once('error', () => finish(false));
    });
  }

  it('no longer honours the desktop-local magic token', async () => {
    expect(await spawned('terminalToken=desktop-local')).toBe(false);
  });

  it('refuses a connection with no credentials', async () => {
    expect(await spawned('')).toBe(false);
  });

  it('opens for the owner', async () => {
    expect(await spawned('', { Cookie: `spaces-desktop=${signDesktop(SECRET, 'session', 60)}` })).toBe(true);
  }, 15000);
});

describe('pages on other localhost ports', () => {
  const cookie = () => `spaces-desktop=${signDesktop(SECRET, 'session', 60)}`;

  it('cannot open the control channel with the owner cookie', async () => {
    expect(await hello('/proto', { Cookie: cookie(), Origin: 'http://localhost:8080' })).toBe('rejected');
  });

  it('cannot even with a valid bearer, since a browser page sent it', async () => {
    expect(await hello('/proto', { Authorization: `Bearer ${SECRET}`, Origin: 'http://localhost:8080' })).toBe('rejected');
  });

  it("while Spaces' own page can", async () => {
    expect(await hello('/proto', { Cookie: cookie(), Origin: `http://127.0.0.1:${port}` })).toBe('ok');
  });

  it('accepts a per-request token from a tool', async () => {
    expect(await hello('/proto', { Authorization: `Bearer ${signDesktop(SECRET, 'api', 60)}` })).toBe('ok');
  });
});

describe('query-string credentials', () => {
  it('the internal secret in ?internal= no longer opens a terminal in desktop mode', async () => {
    const opened = await new Promise<boolean>((resolve) => {
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws?paneId=q-${Date.now()}&internal=${SECRET}`);
      const t = setTimeout(() => { ws.terminate(); resolve(false); }, 6000);
      ws.on('message', (d) => { try { if (JSON.parse(d.toString()).type === 'ready') { clearTimeout(t); ws.terminate(); resolve(true); } } catch {} });
      ws.once('close', () => { clearTimeout(t); resolve(false); });
      ws.once('error', () => { clearTimeout(t); resolve(false); });
    });
    expect(opened).toBe(false);
  }, 10000);
});
