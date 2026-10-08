import { NextRequest, NextResponse } from 'next/server';
import { IS_MULTIUSER } from '@/lib/flags';
import { isPublicAssetPath } from '@/lib/auth/public-paths';
import {
  DESKTOP_COOKIE,
  cameFromSelf,
  isLocalHost,
  isOwnerCredential,
  presentedSecret,
  verifyDesktopSession,
} from '@/lib/auth/desktop';

/** Documentation meant for coding agents to fetch; nothing about the user. */
const DESKTOP_PUBLIC = new Set(['/agent-guide.md', '/llms.txt']);

/**
 * Desktop mode: the owner, and only the owner — see bin/lib/desktop-auth.js.
 * Runs for requests through the front server and for any that reach this
 * internal port directly, which every local account can.
 */
async function desktopGate(request: NextRequest, passThrough: () => NextResponse): Promise<NextResponse> {
  const allowed = (process.env.SPACES_ALLOWED_ORIGINS || '')
    .split(',').map((h) => h.trim()).filter(Boolean);
  if (!isLocalHost(request.headers.get('host'), allowed)) {
    // A rebound hostname: the browser was talking to someone else's page.
    return new NextResponse('Unrecognized host', { status: 421 });
  }
  const pathname = request.nextUrl.pathname;
  if (isPublicAssetPath(pathname) || DESKTOP_PUBLIC.has(pathname)) return passThrough();

  // Fail closed: without the secret nothing can be verified (both checks
  // refuse an empty or short one).
  const secret = process.env.SPACES_INTERNAL_TOKEN || '';
  if (await isOwnerCredential(secret, presentedSecret(request.headers))) return passThrough();
  // The cookie alone counts only for requests from Spaces' own pages: a page
  // on another localhost port is the same site, so the browser attaches the
  // cookie to its requests too.
  if (cameFromSelf(request.headers)
    && await verifyDesktopSession(secret, request.cookies.get(DESKTOP_COOKIE)?.value)) {
    return passThrough();
  }

  if (pathname.startsWith('/api/')) {
    return NextResponse.json(
      { error: 'unauthorized', hint: 'Run "spaces open" on this machine to sign in.' },
      { status: 401 },
    );
  }
  return new NextResponse(LOCKED_PAGE, {
    status: 401,
    headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' },
  });
}

const LOCKED_PAGE = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Spaces</title>
<style>body{font:15px/1.5 system-ui,sans-serif;background:#11111b;color:#cdd6f4;display:grid;place-items:center;min-height:100vh;margin:0}
main{max-width:30rem;padding:2rem}b{color:#6366f1}code{background:#1e1e2e;padding:.15rem .4rem;border-radius:4px}</style>
<main><p><b>&#9670; spaces</b></p><p>This browser is not signed in. In a terminal on this machine, run</p>
<p><code>spaces open</code></p><p>and it will open Spaces here, signed in.</p></main>`;

function timingSafeEqualStr(a: string, b: string): boolean {
  const aBytes = new TextEncoder().encode(a);
  const bBytes = new TextEncoder().encode(b);
  if (aBytes.length !== bBytes.length) return false;
  let result = 0;
  for (let i = 0; i < aBytes.length; i++) result |= aBytes[i] ^ bBytes[i];
  return result === 0;
}

async function verifyToken(token: string, secretHex: string): Promise<{ sub: string; role: string } | null> {
  if (!token) return null;
  const parts = token.split('.');
  if (parts.length !== 2) return null;

  const [payloadB64, sig] = parts;

  const keyBytes = new Uint8Array(secretHex.length / 2);
  for (let i = 0; i < secretHex.length; i += 2) {
    keyBytes[i / 2] = parseInt(secretHex.substring(i, i + 2), 16);
  }
  const key = await crypto.subtle.importKey(
    'raw', keyBytes, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']
  );

  const sigBytes = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(payloadB64));
  const expectedSig = btoa(String.fromCharCode(...new Uint8Array(sigBytes)))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');

  if (!timingSafeEqualStr(expectedSig, sig)) return null;

  try {
    const payloadStr = atob(payloadB64.replace(/-/g, '+').replace(/_/g, '/'));
    const payload = JSON.parse(payloadStr);
    if (payload.exp < Math.floor(Date.now() / 1000)) return null;
    return { sub: payload.sub, role: payload.role || 'user' };
  } catch {
    return null;
  }
}

export async function middleware(request: NextRequest) {
  // Defense-in-depth: strip any incoming x-auth-* headers before forwarding so
  // a route handler can never read attacker-supplied values. Middleware
  // re-sets the canonical x-auth-{user,role} only after validating the
  // session cookie below.
  const cleanHeaders = new Headers(request.headers);
  cleanHeaders.delete('x-auth-user');
  cleanHeaders.delete('x-auth-role');
  const passThrough = () => NextResponse.next({ request: { headers: cleanHeaders } });

  if (!IS_MULTIUSER) {
    return desktopGate(request, passThrough);
  }

  const pathname = request.nextUrl.pathname;

  // Skip auth for login page, specific auth APIs, static assets, and Next.js internals.
  // Static-asset suffixes are matched via isPublicAssetPath, which NEVER bypasses
  // /api/ paths — a bare `pathname.endsWith('.js')` previously let unauthenticated
  // requests reach catch-all API routes like /api/proxy/models/<id>/.../x.js.
  if (
    pathname === '/login' || pathname === '/login/' ||
    pathname.startsWith('/api/tier') ||
    pathname.startsWith('/api/auth/login') ||
    pathname.startsWith('/api/auth/logout') ||
    isPublicAssetPath(pathname)
  ) {
    return passThrough();
  }

  // Federation API routes authenticate via Bearer token. Middleware can't validate
  // the token against network.db in Edge runtime, so we only let through requests
  // that carry a well-formed API-key bearer (shape: "spk_" + base36-ish). The route
  // handler is still responsible for calling requireNetworkAuth() — this bypass is
  // purely a shape check to reject arbitrary strings like "Bearer anything".
  const authHeader = request.headers.get('authorization');
  if (pathname.startsWith('/api/network/') && authHeader?.startsWith('Bearer ')) {
    const bearer = authHeader.slice(7);
    if (/^spk_[0-9a-f]{64}$/.test(bearer)) {
      // Bearer-authed federation request. The handler must call
      // requireNetworkAuth() — middleware does not set x-auth-role, so admin
      // handlers continue to refuse these requests via the requireAdmin guard.
      return passThrough();
    }
  }

  // Connection request/callback endpoints are unauthenticated (verified by token in body)
  if (request.method === 'POST' && (
    pathname === '/api/network/connect-request' || pathname === '/api/network/connect-request/' ||
    pathname === '/api/network/connect-callback' || pathname === '/api/network/connect-callback/'
  )) {
    return passThrough();
  }

  // Internal API requests from MCP servers / agent processes / VR client.
  // Authentication is the constant-time comparison of the SPACES_INTERNAL_TOKEN
  // (32-byte random value, separate from the session-signing secret).
  //
  // SECURITY (review S1): the token is accepted ONLY via the x-spaces-internal
  // header. It used to also be read from a ?_internal= query param, which leaks
  // the bearer secret into access logs, browser history, and Referer headers.
  const internalToken = request.headers.get('x-spaces-internal');
  const expectedInternalToken = process.env.SPACES_INTERNAL_TOKEN || '';
  if (internalToken && expectedInternalToken && timingSafeEqualStr(internalToken, expectedInternalToken)) {
    return passThrough();
  }

  // Validate session cookie
  const sessionToken = request.cookies.get('spaces-session')?.value;
  const secretHex = process.env.SPACES_SESSION_SECRET || '';

  if (!secretHex) {
    const loginUrl = request.nextUrl.clone();
    loginUrl.pathname = '/login';
    loginUrl.searchParams.set('error', 'no-secret');
    return NextResponse.redirect(loginUrl);
  }

  if (!sessionToken) {
    const loginUrl = request.nextUrl.clone();
    loginUrl.pathname = '/login';
    loginUrl.searchParams.set('redirectTo', pathname);
    return NextResponse.redirect(loginUrl);
  }

  const session = await verifyToken(sessionToken, secretHex);
  if (!session) {
    const loginUrl = request.nextUrl.clone();
    loginUrl.pathname = '/login';
    loginUrl.searchParams.set('redirectTo', pathname);
    return NextResponse.redirect(loginUrl);
  }

  // Set canonical auth headers on the forwarded request — these are the only
  // x-auth-* values any handler should ever see.
  cleanHeaders.set('x-auth-user', session.sub);
  cleanHeaders.set('x-auth-role', session.role);

  // Auto-redirect mobile users
  if (pathname === '/' || pathname === '') {
    const ua = request.headers.get('user-agent') || '';
    const isMobile = /Android|webOS|iPhone|iPad|iPod|BlackBerry|IEMobile|Opera Mini/i.test(ua);
    if (isMobile) {
      const mobileUrl = request.nextUrl.clone();
      mobileUrl.pathname = '/m/';
      return NextResponse.redirect(mobileUrl);
    }
  }

  return NextResponse.next({
    request: { headers: cleanHeaders },
  });
}

export const config = {
  matcher: [
    '/',
    '/((?!_next/static|_next/image).*)',
  ],
};
