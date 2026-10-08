import { NextRequest } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '@/lib/network';
import { ensureInitialized } from '@/lib/db/init';
import { getPanesByWorkspace, updateWorkspace, deleteWorkspaceFull } from '@/lib/db/queries';
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
    const panes = getPanesByWorkspace(parseInt(id, 10));
    return Response.json({ panes });
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
    updateWorkspace(parseInt(id, 10), body);
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
    deleteWorkspaceFull(parseInt(id, 10));
    return Response.json({ success: true });
  });
}
