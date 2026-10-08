import { NextRequest, NextResponse } from 'next/server';
import { listApiKeys, createApiKey } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import os from 'os';

export async function GET(request: NextRequest) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const username = request.headers.get('x-auth-user') || os.userInfo().username;
  const keys = listApiKeys(username);
  return NextResponse.json(keys);
}

export async function POST(request: NextRequest) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const { name, permissions } = await request.json() as any;

  if (!name || typeof name !== 'string' || !name.trim()) {
    return NextResponse.json({ error: 'Name is required' }, { status: 400 });
  }

  const username = request.headers.get('x-auth-user') || require('os').userInfo().username;
  const { rawKey, record } = createApiKey(name.trim(), permissions || 'read', username);
  return NextResponse.json({ rawKey, ...record }, { status: 201 });
}
