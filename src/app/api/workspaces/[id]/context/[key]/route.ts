import { NextRequest } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import * as contextByKey from '@/lib/collab/api/contextByKey';

type Ctx = { params: Promise<{ id: string; key: string }> };

export async function GET(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return contextByKey.GET(request, ctx);
  });
}

export async function PUT(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return contextByKey.PUT(request, ctx);
  });
}

export async function DELETE(request: NextRequest, ctx: Ctx) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();
    return contextByKey.DELETE(request, ctx);
  });
}
