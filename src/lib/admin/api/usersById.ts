import { getUserById, updateUser, deleteUser } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import { validateShellUserName } from '../validate';
import { osUserExists } from '../os-users';

function getAuthUser(request: Request): string {
  return request.headers.get('x-auth-user') || require('os').userInfo().username;
}

function formatUser(u: NonNullable<ReturnType<typeof getUserById>>) {
  return {
    id: u.id,
    username: u.username,
    displayName: u.display_name,
    shellUser: u.shell_user,
    role: u.role,
    totpEnabled: !!u.totp_enabled,
    created: u.created,
  };
}

export async function GET(request: Request, { params }: { params: Promise<{ id: string }> }) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const { id } = await params;
  const user = getUserById(id);
  if (!user) {
    return Response.json({ error: 'User not found' }, { status: 404 });
  }

  return Response.json(formatUser(user));
}

export async function PUT(request: Request, { params }: { params: Promise<{ id: string }> }) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const { id } = await params;
  const user = getUserById(id);
  if (!user) {
    return Response.json({ error: 'User not found' }, { status: 404 });
  }

  const body: any = await request.json();
  const updates: Parameters<typeof updateUser>[1] = {};

  if (body.displayName !== undefined) updates.displayName = body.displayName;
  if (body.shellUser !== undefined) {
    // Syntactic + privileged-account validation (reviews L2/M6/L3).
    const shellUserErr = validateShellUserName(body.shellUser);
    if (shellUserErr) return Response.json({ error: shellUserErr }, { status: 400 });
    // Validate that the shell user is a real, selectable OS login account
    // (cross-platform — the old `id` check rejected every user on Windows).
    if (!osUserExists(body.shellUser)) {
      return Response.json(
        { error: `"${body.shellUser}" is not a selectable OS login account on this machine.` },
        { status: 400 }
      );
    }
    updates.shellUser = body.shellUser;
  }
  if (body.role !== undefined) updates.role = body.role;
  if (body.password !== undefined) {
    if (body.password.length < 8) {
      return Response.json({ error: 'Password must be at least 8 characters' }, { status: 400 });
    }
    updates.password = body.password;
  }
  if (body.totpReset) {
    updates.totpSecret = null;
    updates.totpEnabled = false;
  }

  updateUser(id, updates);

  const updated = getUserById(id);
  return Response.json(formatUser(updated!));
}

export async function DELETE(request: Request, { params }: { params: Promise<{ id: string }> }) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const { id } = await params;
  const user = getUserById(id);
  if (!user) {
    return Response.json({ error: 'User not found' }, { status: 404 });
  }

  const currentUser = getAuthUser(request);
  if (user.username === currentUser) {
    return Response.json({ error: 'Cannot delete your own account' }, { status: 400 });
  }

  deleteUser(id);
  return Response.json({ ok: true });
}
