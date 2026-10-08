import { listUsers, createUser } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import { validateUsername, validateShellUserName } from '../validate';
import { osUserExists } from '../os-users';

export async function GET(request: Request) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const users = listUsers().map(u => ({
    id: u.id,
    username: u.username,
    displayName: u.display_name,
    shellUser: u.shell_user,
    role: u.role,
    totpEnabled: !!u.totp_enabled,
    created: u.created,
  }));

  return Response.json(users);
}

export async function POST(request: Request) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  try {
    const body: any = await request.json();
    const { username, password, displayName, shellUser, role } = body;

    if (!username || !password || !shellUser) {
      return Response.json({ error: 'username, password, and shellUser are required' }, { status: 400 });
    }

    // Syntactic validation (review M6: username becomes a filesystem path;
    // review L2: shell user must not be a privileged account).
    const usernameErr = validateUsername(username);
    if (usernameErr) return Response.json({ error: usernameErr }, { status: 400 });
    const shellUserErr = validateShellUserName(shellUser);
    if (shellUserErr) return Response.json({ error: shellUserErr }, { status: 400 });

    if (password.length < 8) {
      return Response.json({ error: 'Password must be at least 8 characters' }, { status: 400 });
    }

    // Validate that the shell user is a real, selectable OS login account
    // (cross-platform — the old `id` check rejected every user on Windows).
    if (!osUserExists(shellUser)) {
      return Response.json(
        { error: `"${shellUser}" is not a selectable OS login account on this machine.` },
        { status: 400 }
      );
    }

    const user = createUser({
      username,
      password,
      displayName: displayName || username,
      shellUser,
      role: role || 'user',
    });

    return Response.json({
      id: user.id,
      username: user.username,
      displayName: user.display_name,
      shellUser: user.shell_user,
      role: user.role,
      totpEnabled: !!user.totp_enabled,
      created: user.created,
    });
  } catch (err: any) {
    if (err.message?.includes('UNIQUE constraint')) {
      return Response.json({ error: 'Username already exists' }, { status: 409 });
    }
    console.error('[Admin Users POST]', err);
    return Response.json({ error: 'Internal server error' }, { status: 500 });
  }
}
