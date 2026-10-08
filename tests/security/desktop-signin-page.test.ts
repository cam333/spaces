import { describe, it, expect } from 'vitest';
import { signedInPage } from '../../bin/lib/desktop-auth.js';

// A redeemed sign-in link answers with a page, not a redirect: a redirect
// from a cross-site start (a local file, another app) would arrive without
// the SameSite=Strict cookie it just set.

function capture() {
  const out: any = { status: 0, headers: {}, body: '' };
  return {
    out,
    res: {
      writeHead(status: number, headers: Record<string, string>) { out.status = status; out.headers = headers; },
      end(body: string) { out.body = body; },
    },
  };
}

describe('signedInPage', () => {
  it('sets the cookie and moves on to the app from a page of its own', () => {
    const { out, res } = capture();
    signedInPage(res, 'spaces-desktop=abc; HttpOnly; SameSite=Strict', '/base/');
    expect(out.status).toBe(200);
    expect(out.headers['Set-Cookie']).toContain('spaces-desktop=abc');
    expect(out.headers.Location).toBeUndefined();
    expect(out.headers['Content-Security-Policy']).toBe("default-src 'none'");
    expect(out.headers['Cache-Control']).toBe('no-store');
    expect(out.body).toContain('<meta http-equiv="refresh" content="0;url=/base/">');
  });

  it('escapes the target', () => {
    const { out, res } = capture();
    signedInPage(res, 'c=1', '/x"><script>');
    expect(out.body).not.toContain('<script>');
    expect(out.body).toContain('/x&quot;>&lt;script>');
  });
});
