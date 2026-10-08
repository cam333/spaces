import { getUser } from '../../admin/db';

export async function GET(request: Request) {
  const username = request.headers.get('x-auth-user');
  const role = request.headers.get('x-auth-role') || 'user';

  if (!username) {
    return Response.json({ error: 'Not authenticated' }, { status: 401 });
  }

  try {
    const user = getUser(username);
    return Response.json({
      username,
      role,
      displayName: user?.display_name || username,
    });
  } catch {
    return Response.json({
      username,
      role,
      displayName: username,
    });
  }
}
