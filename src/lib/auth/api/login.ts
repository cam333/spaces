import { createRequire } from 'module';
import { getUser, verifyPassword, updateUser, recordLogin, setPendingTotp, getPendingTotp, clearPendingTotp } from '../../admin/db';
import { issueSessionToken, setSessionCookie } from '../session';
import { generateSecret, verifyCode } from '../totp';
import { authStore, clientIp, checkRateLimit, recordFailure, recordSuccess } from '../rate-limit';

const require_ = createRequire(__filename);
const QRCode = require_('qrcode');

export async function POST(request: Request) {
  try {
    const body: any = await request.json();
    const { username, password, totpCode } = body;

    if (!username || !password) {
      return Response.json({ error: 'Username and password required' }, { status: 400 });
    }

    // Brute-force throttling (review H6). Always limit per-account; also limit
    // per-IP when a trusted proxy identifies the source. Deny if either is
    // locked. Failures are recorded on every wrong password/TOTP; success
    // clears the counters.
    const store = authStore();
    const ip = clientIp(request);
    const keys = [`user:${username}`, ...(ip !== 'unknown' ? [`ip:${ip}`] : [])];
    for (const key of keys) {
      const rl = checkRateLimit(store, key);
      if (!rl.allowed) {
        return Response.json(
          { error: 'Too many attempts. Please try again later.' },
          { status: 429, headers: { 'Retry-After': String(Math.ceil((rl.retryAfterMs ?? 0) / 1000)) } },
        );
      }
    }
    const fail = () => { for (const k of keys) recordFailure(store, k); };
    const succeed = () => { for (const k of keys) recordSuccess(store, k); };

    const user = getUser(username);
    if (!user || !verifyPassword(password, user.password_hash)) {
      fail();
      return Response.json({ error: 'Invalid username or password' }, { status: 401 });
    }

    if (!user.totp_enabled) {
      if (!totpCode) {
        // Generate the enrollment secret AND persist it server-side (review M8).
        // We still return it to display the QR, but verification below uses the
        // SERVER-stored secret — never a value echoed back by the client.
        const { secret, uri } = generateSecret(username);
        setPendingTotp(username, secret);
        const qrDataUrl = await QRCode.toDataURL(uri);
        return Response.json({
          status: 'setup-totp',
          setupSecret: secret,
          qrDataUrl,
        });
      }

      // review M8: verify against the server-stored pending secret; ignore any
      // client-supplied setupSecret so an attacker cannot enroll a chosen secret.
      const pendingSecret = getPendingTotp(username);
      if (!pendingSecret) {
        return Response.json({ error: 'TOTP enrollment expired — please restart sign-in' }, { status: 400 });
      }

      if (!verifyCode(pendingSecret, totpCode)) {
        fail();
        return Response.json({ error: 'Invalid TOTP code' }, { status: 401 });
      }

      updateUser(user.id, { totpSecret: pendingSecret, totpEnabled: true });
      clearPendingTotp(username);

      succeed();
      const token = issueSessionToken(username, user.role, user.token_version);
      const response = Response.json({ status: 'ok' });
      setSessionCookie(response, token);
      try { recordLogin(username, request.headers.get('x-forwarded-for') || '', request.headers.get('user-agent') || ''); } catch {}
      return response;
    }

    if (!totpCode) {
      return Response.json({ status: 'totp-required' });
    }

    if (!verifyCode(user.totp_secret!, totpCode)) {
      fail();
      return Response.json({ error: 'Invalid TOTP code' }, { status: 401 });
    }

    succeed();
    const token = issueSessionToken(username, user.role, user.token_version);
    const response = Response.json({ status: 'ok' });
    setSessionCookie(response, token);
    try { recordLogin(username, request.headers.get('x-forwarded-for') || '', request.headers.get('user-agent') || ''); } catch {}
    return response;

  } catch (err: any) {
    console.error('[Auth Login]', err);
    return Response.json({ error: 'Internal server error' }, { status: 500 });
  }
}
