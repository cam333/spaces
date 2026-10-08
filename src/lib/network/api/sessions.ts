import { NextRequest, NextResponse } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '../auth';

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

  // This handler needs the host app's DB access - return empty for now
  return NextResponse.json({ sessions: [], total: 0 });
}
