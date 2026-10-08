import { NextRequest } from 'next/server';
import { randomUUID } from 'crypto';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { getPanesByWorkspace, getActivePanes, createPane } from '@/lib/db/queries';
import { withUser } from '@/lib/auth';

export async function GET(req: NextRequest) {
  let keyRecord;
  try { keyRecord = requireNetworkAuth(req); }
  catch (e: any) {
    if (e instanceof NetworkAuthError) return Response.json({ error: e.message }, { status: 401 });
    throw e;
  }

  const keyUser = keyRecord?.username;
  if (!keyUser) return Response.json({ error: 'API key missing user binding' }, { status: 401 });

  return withUser(keyUser, async () => {
    await ensureInitialized();
    const wsId = req.nextUrl.searchParams.get('workspace_id');
    const panes = wsId ? getPanesByWorkspace(parseInt(wsId, 10)) : getActivePanes();
    return Response.json({ panes });
  });
}

export async function POST(req: NextRequest) {
  let keyRecord;
  try { keyRecord = requireNetworkAuth(req); }
  catch (e: any) {
    if (e instanceof NetworkAuthError) return Response.json({ error: e.message }, { status: 401 });
    throw e;
  }
  if (keyRecord?.permissions !== 'admin') return Response.json({ error: 'Admin permission required' }, { status: 403 });

  const keyUser = keyRecord?.username;
  if (!keyUser) return Response.json({ error: 'API key missing user binding' }, { status: 401 });

  return withUser(keyUser, async () => {
    await ensureInitialized();
    const body = await req.json();
    const pane = createPane({
      id: body.id || randomUUID(),
      title: body.title || 'Terminal',
      color: body.color || '#6366f1',
      cwd: body.cwd || '~',
      agentType: body.agentType || 'shell',
      workspaceId: body.workspaceId,
    });
    return Response.json(pane);
  });
}
