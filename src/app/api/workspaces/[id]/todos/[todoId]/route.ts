import { NextRequest, NextResponse } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import { updateWorkspaceTodo, deleteWorkspaceTodo } from '@/lib/db/queries';

export async function PATCH(
  request: NextRequest,
  { params }: { params: Promise<{ id: string; todoId: string }> }
) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    const { id, todoId } = await params;
    const wsId = parseInt(id, 10);
    const tid = parseInt(todoId, 10);
    if (!Number.isFinite(wsId) || !Number.isFinite(tid)) {
      return NextResponse.json({ error: 'invalid workspace or todo id' }, { status: 400 });
    }
    const body = await request.json().catch(() => null);
    if (!body || (body.text === undefined && body.completed === undefined)) {
      return NextResponse.json({ error: 'text or completed required' }, { status: 400 });
    }
    try {
      const updated = updateWorkspaceTodo(wsId, tid, {
        text: typeof body.text === 'string' ? body.text : undefined,
        completed: typeof body.completed === 'boolean' ? body.completed : undefined,
      });
      if (!updated) {
        return NextResponse.json({ error: 'not found' }, { status: 404 });
      }
      return NextResponse.json(updated);
    } catch (err) {
      const message = err instanceof Error ? err.message : 'update failed';
      return NextResponse.json({ error: message }, { status: 400 });
    }
  });
}

export async function DELETE(
  request: NextRequest,
  { params }: { params: Promise<{ id: string; todoId: string }> }
) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    const { id, todoId } = await params;
    const wsId = parseInt(id, 10);
    const tid = parseInt(todoId, 10);
    if (!Number.isFinite(wsId) || !Number.isFinite(tid)) {
      return NextResponse.json({ error: 'invalid workspace or todo id' }, { status: 400 });
    }
    deleteWorkspaceTodo(wsId, tid);
    return NextResponse.json({ ok: true });
  });
}
