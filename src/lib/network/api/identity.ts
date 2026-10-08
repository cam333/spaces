import { NextRequest, NextResponse } from 'next/server';
import { getNodeIdentity, setNodeName } from '../identity';
import { requireAdmin } from '@/lib/auth/guard';

// GET is intentionally public (used by remote nodes during handshake).
// PUT is admin-only — sets the local node's display name in network.db.

export async function GET() {
  const identity = getNodeIdentity();
  return NextResponse.json(identity);
}

export async function PUT(request: NextRequest) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const { name } = await request.json() as any;
  if (!name || typeof name !== 'string' || !name.trim()) {
    return NextResponse.json({ error: 'Name is required' }, { status: 400 });
  }
  setNodeName(name.trim());
  const identity = getNodeIdentity();
  return NextResponse.json(identity);
}
