import { NextRequest } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { getAllProjects } from '@/lib/db/queries';
import { withUser } from '@/lib/auth';

export async function GET(req: NextRequest) {
  let keyRecord;
  try {
    keyRecord = requireNetworkAuth(req);
  } catch (e: any) {
    if (e instanceof NetworkAuthError) {
      return Response.json({ error: e.message }, { status: 401 });
    }
    throw e;
  }

  const keyUser = keyRecord?.username;
  if (!keyUser) return Response.json({ error: 'API key missing user binding' }, { status: 401 });

  return withUser(keyUser, async () => {
    await ensureInitialized();
    const projects = getAllProjects();
    return Response.json(projects);
  });
}
