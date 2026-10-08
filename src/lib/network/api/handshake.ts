import { NextRequest, NextResponse } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '../auth';
import { getNodeIdentity } from '../identity';

export async function GET(request: NextRequest) {
  let keyRecord;
  try {
    keyRecord = requireNetworkAuth(request);
  } catch (e) {
    if (e instanceof NetworkAuthError) {
      return NextResponse.json({ error: e.message }, { status: 401 });
    }
    throw e;
  }

  const identity = getNodeIdentity();

  return NextResponse.json({
    nodeId: identity.nodeId,
    nodeName: identity.nodeName,
    version: identity.version,
    // Counts are injected by the host app's route handler
    sessionCount: 0,
    projectCount: 0,
    workspaceCount: 0,
  });
}
