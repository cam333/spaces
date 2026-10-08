import { getUser } from '../../admin/db';
import { verifyCode, issueTerminalToken } from '../totp';
import { issueSessionToken, setSessionCookie } from '../session';
import { getAuthUserOrNull } from '../context';

export async function POST(request: Request) {
  // Require an authenticated session in multiuser mode — this endpoint mints a
  // session cookie + terminal token, so it must never run as the OS-user
  // fallback for an unauthenticated caller (review H2).
  const username = getAuthUserOrNull(request);
  if (!username) {
    return Response.json({ success: false, error: 'Authentication required' }, { status: 401 });
  }
  const body: any = await request.json();
  const { code, action } = body;

  const user = getUser(username);
  if (!user || !user.totp_enabled || !user.totp_secret) {
    return Response.json({ success: false, error: 'TOTP not configured' }, { status: 400 });
  }

  if (!verifyCode(user.totp_secret, code)) {
    return Response.json({ success: false, error: 'Invalid code' }, { status: 401 });
  }

  // Issue a terminal token for WebSocket auth
  const terminalToken = issueTerminalToken(username);

  // Also issue a session cookie
  const sessionToken = issueSessionToken(username, user.role, user.token_version);
  const response = Response.json({ success: true, terminalToken });
  setSessionCookie(response, sessionToken);
  return response;
}
