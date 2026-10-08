import { requireAdmin } from '@/lib/auth/guard';
import { listOsUsers } from '../os-users';

// Admin-only: list real OS login accounts so the user form can pin the shell
// mapping to an actual machine user.
export async function GET(request: Request) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  return Response.json({ users: listOsUsers() });
}
