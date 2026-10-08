import { NextRequest } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { searchSessions } from '@/lib/db/queries';
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
    const query = searchParams.get('q') || '';
    if (!query) {
      return Response.json({ results: [], query: '' });
    }

    const params = {
      projectId: searchParams.get('projectId') || undefined,
      limit: parseInt(searchParams.get('limit') || '20', 10),
      offset: parseInt(searchParams.get('offset') || '0', 10),
    };

    const results = searchSessions(query, params);
    return Response.json({ results, query });
  });
}
