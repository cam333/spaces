import { NextRequest } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { getPaneById, updatePane, deletePane } from '@/lib/db/queries';
import { withUser } from '@/lib/auth';

export async function GET(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  let keyRecord;
  try { keyRecord = requireNetworkAuth(req); }
  catch (e: any) {
    if (e instanceof NetworkAuthError) return Response.json({ error: e.message }, { status: 401 });
    throw e;
  }

  const keyUser = keyRecord?.username;
  if (!keyUser) return Response.json({ error: 'API key missing user binding' }, { status: 401 });

  const { id } = await params;
  return withUser(keyUser, async () => {
    await ensureInitialized();
    const pane = getPaneById(id);
    if (!pane) return Response.json({ error: 'Pane not found' }, { status: 404 });
    return Response.json(pane);
  });
}

export async function PUT(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  let keyRecord;
  try { keyRecord = requireNetworkAuth(req); }
  catch (e: any) {
    if (e instanceof NetworkAuthError) return Response.json({ error: e.message }, { status: 401 });
    throw e;
  }
  if (keyRecord?.permissions !== 'admin') return Response.json({ error: 'Admin permission required' }, { status: 403 });

  const keyUser = keyRecord?.username;
  if (!keyUser) return Response.json({ error: 'API key missing user binding' }, { status: 401 });

  const { id } = await params;
  return withUser(keyUser, async () => {
    await ensureInitialized();
    const body = await req.json();
    updatePane(id, body);
    return Response.json({ success: true });
  });
}

export async function DELETE(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  let keyRecord;
  try { keyRecord = requireNetworkAuth(req); }
  catch (e: any) {
    if (e instanceof NetworkAuthError) return Response.json({ error: e.message }, { status: 401 });
    throw e;
  }
  if (keyRecord?.permissions !== 'admin') return Response.json({ error: 'Admin permission required' }, { status: 403 });

  const keyUser = keyRecord?.username;
  if (!keyUser) return Response.json({ error: 'API key missing user binding' }, { status: 401 });

  const { id } = await params;
  return withUser(keyUser, async () => {
    await ensureInitialized();
    deletePane(id);
    return Response.json({ success: true });
  });
}
