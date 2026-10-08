import { NextRequest, NextResponse } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import { getWorkspaceTodos, addWorkspaceTodo } from '@/lib/db/queries';

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ id: string }> }
) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    const { id } = await params;
    const wsId = parseInt(id, 10);
    if (!Number.isFinite(wsId)) {
      return NextResponse.json({ error: 'invalid workspace id' }, { status: 400 });
    }
    const todos = getWorkspaceTodos(wsId);
    return NextResponse.json(todos);
  });
}

export async function POST(
  request: NextRequest,
  { params }: { params: Promise<{ id: string }> }
) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    const { id } = await params;
    const wsId = parseInt(id, 10);
    if (!Number.isFinite(wsId)) {
      return NextResponse.json({ error: 'invalid workspace id' }, { status: 400 });
    }
    const body = await request.json().catch(() => null);
    const text = typeof body?.text === 'string' ? body.text : '';
    if (!text.trim()) {
      return NextResponse.json({ error: 'text is required' }, { status: 400 });
    }
    const todo = addWorkspaceTodo(wsId, text);
    return NextResponse.json(todo, { status: 201 });
  });
}
