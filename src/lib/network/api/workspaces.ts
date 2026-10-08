import { NextRequest, NextResponse } from 'next/server';
import { requireNetworkAuth, NetworkAuthError } from '../auth';

export async function GET(request: NextRequest) {
  try {
    requireNetworkAuth(request);
  } catch (e) {
    if (e instanceof NetworkAuthError) {
      return NextResponse.json({ error: e.message }, { status: 401 });
    }
    throw e;
  }

  return NextResponse.json({ workspaces: [], total: 0 });
}
