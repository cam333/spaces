import { NextResponse } from 'next/server';
import { getDiscoveredNodes } from '../db';
import { requireAdmin } from '@/lib/auth/guard';

export async function GET(request: Request) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const nodes = getDiscoveredNodes();
  return NextResponse.json(nodes);
}
