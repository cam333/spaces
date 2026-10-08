import { getUser } from '../../admin/db';
import { getAuthUserOrNull } from '../context';

export async function GET(request: Request) {
  const username = getAuthUserOrNull(request);
  if (!username) {
    return Response.json({ error: 'Authentication required' }, { status: 401 });
  }
  const user = getUser(username);
  return Response.json({
    enabled: !!user?.totp_enabled,
    required: true,
  });
}
