import { createHash } from 'crypto';
import type { Utilization } from '@/types/claude';

const ANTHROPIC_USAGE_URL = 'https://api.anthropic.com/api/oauth/usage';
const REQUEST_TIMEOUT_MS = 5_000;
const CACHE_TTL_MS = 30_000;

// UA + anthropic-beta header match Claude Code's. The endpoint requires the
// beta header; see claude-code/src/utils/http.ts (getAuthHeaders) and
// claude-code/src/constants/oauth.ts (OAUTH_BETA_HEADER).
const USER_AGENT = 'claude-code/spaces';
const OAUTH_BETA_HEADER = 'oauth-2025-04-20';

interface CacheEntry {
  fetchedAt: number;
  data: Utilization;
}

const cache = new Map<string, CacheEntry>();

function cacheKey(accessToken: string): string {
  // Full SHA-256 of the token so two tokens sharing a prefix cannot collide.
  return createHash('sha256').update(accessToken).digest('hex');
}

export type FetchOutcome =
  | { ok: true; data: Utilization }
  | { ok: false; reason: 'upstream_error' };

export async function fetchUtilization(accessToken: string): Promise<FetchOutcome> {
  const key = cacheKey(accessToken);
  const cached = cache.get(key);
  if (cached && Date.now() - cached.fetchedAt < CACHE_TTL_MS) {
    return { ok: true, data: cached.data };
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const res = await fetch(ANTHROPIC_USAGE_URL, {
      method: 'GET',
      headers: {
        'Authorization': `Bearer ${accessToken}`,
        'anthropic-beta': OAUTH_BETA_HEADER,
        'Content-Type': 'application/json',
        'User-Agent': USER_AGENT,
      },
      signal: controller.signal,
    });

    if (!res.ok) {
      return { ok: false, reason: 'upstream_error' };
    }

    const data = (await res.json()) as Utilization;
    cache.set(key, { fetchedAt: Date.now(), data });
    return { ok: true, data };
  } catch {
    return { ok: false, reason: 'upstream_error' };
  } finally {
    clearTimeout(timer);
  }
}

/** Testing/hot-reload helper. */
export function __clearUsageCache() {
  cache.clear();
}
