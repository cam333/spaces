import { NextRequest } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { getSessions, getWorkspacesForSession } from '@/lib/db/queries';
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

    const { searchParams } = req.nextUrl;
    const params = {
      projectId: searchParams.get('projectId') || undefined,
      projectPath: searchParams.get('projectPath') || undefined,
      search: searchParams.get('search') || undefined,
      sortBy: searchParams.get('sortBy') || 'modified',
      sortDir: searchParams.get('sortDir') || 'DESC',
      offset: parseInt(searchParams.get('offset') || '0', 10),
      limit: parseInt(searchParams.get('limit') || '50', 10),
    };

    const result = getSessions(params);

    const sessions = result.sessions.map(s => ({
      ...s,
      workspaces: getWorkspacesForSession(s.id),
    }));

    return Response.json({ sessions, total: result.total });
  });
}
