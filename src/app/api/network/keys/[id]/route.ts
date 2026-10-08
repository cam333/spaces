import { NextRequest } from 'next/server';
import * as keysById from '@/lib/network/api/keysById';

export async function DELETE(req: NextRequest, ctx: { params: Promise<{ id: string }> }) {
  return keysById.DELETE(req, ctx);
}
