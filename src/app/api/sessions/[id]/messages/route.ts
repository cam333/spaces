import { NextRequest, NextResponse } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import { getSessionById } from '@/lib/db/queries';
import { readMessages } from '@/lib/claude/parser';
import { readGeminiMessages } from '@/lib/gemini/parser';

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ id: string }> }
) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    const { id } = await params;
    const { searchParams } = request.nextUrl;

    const offset = Math.max(0, parseInt(searchParams.get('offset') || '0', 10) || 0);
    const limit = Math.min(500, Math.max(1, parseInt(searchParams.get('limit') || '50', 10) || 50));

    const session = getSessionById(id);
    if (!session) {
      return NextResponse.json({ error: 'Session not found' }, { status: 404 });
    }

    if (session.agentType === 'gemini') {
      const result = await readGeminiMessages(session.fullPath, offset, limit);
      return NextResponse.json(result);
    }

    const result = await readMessages(session.fullPath, offset, limit);
    return NextResponse.json(result);
  });
}
