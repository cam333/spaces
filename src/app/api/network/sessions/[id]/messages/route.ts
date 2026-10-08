import { NextRequest } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { getSessionById } from '@/lib/db/queries';
import { readMessages } from '@/lib/claude/parser';
import { withUser } from '@/lib/auth';

export async function GET(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
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

  const { id } = await params;
  return withUser(keyUser, async () => {
    await ensureInitialized();
    const { searchParams } = req.nextUrl;
    const offset = parseInt(searchParams.get('offset') || '0', 10);
    const limit = parseInt(searchParams.get('limit') || '50', 10);

    const session = getSessionById(id);
    if (!session) {
      return Response.json({ error: 'Session not found' }, { status: 404 });
    }

    const result = await readMessages(session.fullPath, offset, limit);
    return Response.json(result);
  });
}
