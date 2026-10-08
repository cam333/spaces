import { NextRequest } from 'next/server';
import * as handshake from '@/lib/network/api/handshake';
import { getDb } from '@/lib/db/schema';

export async function GET(req: NextRequest) {
  const response = await handshake.GET(req);
  if (!response.ok) return response;

  // Inject real counts from the host app's DB
  const data = await response.json();
  try {
    const db = getDb();
    data.sessionCount = (db.prepare('SELECT COUNT(*) as c FROM sessions').get() as any)?.c || 0;
    data.projectCount = (db.prepare('SELECT COUNT(*) as c FROM projects').get() as any)?.c || 0;
    data.workspaceCount = (db.prepare('SELECT COUNT(*) as c FROM workspaces').get() as any)?.c || 0;
  } catch {}

  return Response.json(data);
}
