import { NextRequest, NextResponse } from 'next/server';
import os from 'os';
import { IS_MULTIUSER } from '@/lib/flags';
import * as me from '@/lib/auth/api/me';

export async function GET(request: NextRequest) {
  if (!IS_MULTIUSER) {
    // Desktop mode: return local OS user as admin
    return NextResponse.json({
      username: os.userInfo().username,
      role: 'admin',
      displayName: os.userInfo().username,
    });
  }

  return me.GET(request);
}
