import { NextRequest } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import * as context from '@/lib/collab/api/context';

type Ctx = { params: Promise<{ id: string }> };

export async function GET(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return context.GET(request, ctx);
  });
}

export async function DELETE(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return context.DELETE(request, ctx);
  });
}
