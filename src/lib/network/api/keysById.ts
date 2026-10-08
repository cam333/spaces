import { NextRequest, NextResponse } from 'next/server';
import { revokeApiKey, getNetworkDb } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import os from 'os';

export async function DELETE(request: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const { id } = await params;
  const username = request.headers.get('x-auth-user') || os.userInfo().username;

  const db = getNetworkDb();
  const key = db.prepare('SELECT username FROM api_keys WHERE id = ?').get(id) as { username: string | null } | undefined;
  if (!key) {
    return NextResponse.json({ error: 'Key not found' }, { status: 404 });
  }
  if (key.username && key.username !== username) {
    return NextResponse.json({ error: 'Not authorized to revoke this key' }, { status: 403 });
  }

  revokeApiKey(id);
  return NextResponse.json({ ok: true });
}
