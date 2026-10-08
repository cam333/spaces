import { NextRequest } from 'next/server';
import * as nodesById from '@/lib/network/api/nodesById';

export async function PUT(req: NextRequest, ctx: { params: Promise<{ id: string }> }) {
  return nodesById.PUT(req, ctx);
}

export async function DELETE(req: NextRequest, ctx: { params: Promise<{ id: string }> }) {
  return nodesById.DELETE(req, ctx);
}
