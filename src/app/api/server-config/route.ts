import { NextRequest, NextResponse } from 'next/server';
import { requireAdmin } from '@/lib/auth/guard';
import { readServerConfig, writeServerConfig } from '@/lib/server-config';

// Server-level config (port, basePath, multiuser) lives in ~/.spaces/server.json
// and is read by the launcher at boot. This route exposes a small, safe subset
// and lets the local user flip multiuser mode. A restart is required for the
// change to take effect — see the "Restart required" banner in Settings.
//
// Authorization: requireAdmin() in multiuser mode requires a validated admin
// session; in desktop mode it requires the request to come from loopback (so a
// LAN attacker cannot flip the victim into multiuser mode and seed a backdoor).

export async function GET(req: NextRequest) {
  const denied = requireAdmin(req);
  if (denied) return denied;
  const { port, basePath, multiuser } = readServerConfig();
  return NextResponse.json({
    port: port ?? 3457,
    basePath: basePath ?? '',
    multiuser: !!multiuser,
  });
}

export async function PATCH(req: NextRequest) {
  const denied = requireAdmin(req);
  if (denied) return denied;
  const body = await req.json().catch(() => ({}));
  const updates: Record<string, unknown> = {};
  if (typeof body.multiuser === 'boolean') updates.multiuser = body.multiuser;
  const config = writeServerConfig(updates);
  return NextResponse.json({
    port: config.port ?? 3457,
    basePath: config.basePath ?? '',
    multiuser: !!config.multiuser,
  });
}
