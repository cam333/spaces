import { NextRequest } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import * as messageById from '@/lib/collab/api/messageById';

type Ctx = { params: Promise<{ id: string; msgId: string }> };

export async function PATCH(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return messageById.PATCH(request, ctx);
  });
}
