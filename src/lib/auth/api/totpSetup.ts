import { createRequire } from 'module';
import { generateSecret } from '../totp';
import { getAuthUserOrNull } from '../context';

const require_ = createRequire(__filename);
const QRCode = require_('qrcode');

export async function POST(request: Request) {
  const user = getAuthUserOrNull(request);
  if (!user) {
    return Response.json({ error: 'Authentication required' }, { status: 401 });
  }
  const { secret, uri } = generateSecret(user);
  const qrDataUrl = await QRCode.toDataURL(uri);
  return Response.json({ qrDataUrl, secret, uri });
}
