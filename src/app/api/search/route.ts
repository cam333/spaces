import { NextRequest, NextResponse } from 'next/server';
import { getAuthUser, withUser } from '@/lib/auth';
import { ensureInitialized } from '@/lib/db/init';
import { searchSessions } from '@/lib/db/queries';
import { aggregateSearch } from '@/lib/network';

export async function GET(request: NextRequest) {
  const user = getAuthUser(request);
  return withUser(user, async () => {
    await ensureInitialized();

    const { searchParams } = request.nextUrl;
    const q = searchParams.get('q');

    if (!q || q.trim().length < 2) {
      return NextResponse.json({ results: [], query: q });
    }

    const projectId = searchParams.get('projectId') || undefined;
    const limit = Math.min(200, Math.max(1, parseInt(searchParams.get('limit') || '20', 10) || 20));
    const offset = Math.max(0, parseInt(searchParams.get('offset') || '0', 10) || 0);

    const localResults = searchSessions(q, { projectId, limit, offset });

    // If nodes=all, aggregate from remote nodes
    if (searchParams.get('nodes') === 'all') {
      try {
        const remoteParams: Record<string, string> = {};
        if (projectId) remoteParams.projectId = projectId;
        remoteParams.limit = String(limit);

        const aggregated = await aggregateSearch(localResults, q, remoteParams);
        return NextResponse.json({
          results: aggregated.results,
          query: q,
          errors: aggregated.errors,
        });
      } catch {
        // Aggregation failed, return local only
      }
    }

    return NextResponse.json({ results: localResults, query: q });
  });
}
