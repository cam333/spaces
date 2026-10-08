import { NextRequest, NextResponse } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '../auth';
import { getNodeIdentity } from '../identity';

export async function GET(request: NextRequest) {
  try {
    requireNetworkAuth(request);
  } catch (e) {
    if (e instanceof NetworkAuthError) {
      return NextResponse.json({ error: e.message }, { status: 401 });
    }
    throw e;
  }

  const identity = getNodeIdentity();
  return NextResponse.json({
    status: 'ok',
    nodeId: identity.nodeId,
    nodeName: identity.nodeName,
    version: identity.version,
    uptime: process.uptime(),
  });
}
