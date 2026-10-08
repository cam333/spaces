import { clearSessionCookie, verifySessionToken } from '../session';
import { bumpTokenVersion } from '../../admin/db';

export async function POST(request?: Request) {
  // review M7: truly revoke the session, not just clear the browser cookie —
  // bump the user's token version so the (possibly copied) token stops
  // validating at the Node-side guards.
  try {
    const cookie = request?.headers.get('cookie') || '';
    const m = cookie.match(/(?:^|;\s*)spaces-session=([^;]+)/);
    if (m) {
      const session = verifySessionToken(decodeURIComponent(m[1]));
      if (session) bumpTokenVersion(session.sub);
    }
  } catch { /* best-effort; still clear the cookie below */ }

  const response = Response.json({ ok: true });
  clearSessionCookie(response);
  return response;
}
