import { NextRequest } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import * as messages from '@/lib/collab/api/messages';

type Ctx = { params: Promise<{ id: string }> };

export async function GET(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return messages.GET(request, ctx);
  });
}

export async function POST(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return messages.POST(request, ctx);
  });
}

export async function DELETE(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return messages.DELETE(request, ctx);
  });
}
