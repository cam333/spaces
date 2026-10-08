import { NextRequest, NextResponse } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import { readClaudeAiOauthTokens, isTokenExpired } from '@/lib/claude/credentials';
import { fetchUtilization } from '@/lib/claude/usage';
import type { UsageResponse } from '@/types/claude';

export async function GET(request: NextRequest) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();

    const tokens = await readClaudeAiOauthTokens();
    if (!tokens) {
      const body: UsageResponse = { status: 'unauthenticated' };
      return NextResponse.json(body);
    }

    if (isTokenExpired(tokens)) {
      const body: UsageResponse = { status: 'expired' };
      return NextResponse.json(body);
    }

    const outcome = await fetchUtilization(tokens.accessToken);
    if (!outcome.ok) {
      const body: UsageResponse = { status: 'upstream_error' };
      return NextResponse.json(body);
    }

    const body: UsageResponse = { status: 'ok', data: outcome.data };
    return NextResponse.json(body);
  });
}
