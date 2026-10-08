import { NextRequest, NextResponse } from 'next/server';
import { getNode, getNodeApiKey } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import { rejectPrivateUrl, sanitizeProxyPath } from '../url-guard';

type Params = { params: Promise<{ nodeId: string; path: string[] }> };

async function proxyRequest(request: NextRequest, ctx: Params, method: 'GET' | 'POST') {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const { nodeId, path: pathSegments } = await ctx.params;

  const node = getNode(nodeId);
  if (!node) {
    return NextResponse.json({ error: 'Node not found' }, { status: 404 });
  }

  // SECURITY (review H5): re-validate the stored URL at egress — it may have
  // been set before validation existed, or via a path that didn't validate.
  const reject = rejectPrivateUrl(node.url);
  if (reject) {
    return NextResponse.json({ error: 'Node URL is not permitted' }, { status: 400 });
  }

  // SECURITY: reject path traversal / separator tricks so the caller cannot
  // escape the /api/network/ prefix this proxy is meant to forward to.
  const safePath = sanitizeProxyPath(pathSegments);
  if (safePath === null) {
    return NextResponse.json({ error: 'Invalid path' }, { status: 400 });
  }

  const apiKey = getNodeApiKey(nodeId);
  if (!apiKey) {
    return NextResponse.json({ error: 'No API key for node' }, { status: 500 });
  }

  const remoteUrl = `${node.url}/api/network/${safePath}${request.nextUrl.search}`;

  try {
    const init: RequestInit = {
      method,
      headers: {
        'Authorization': `Bearer ${apiKey}`,
        ...(method === 'POST' ? { 'Content-Type': 'application/json' } : {}),
      },
      signal: AbortSignal.timeout(15000),
    };
    if (method === 'POST') init.body = await request.text();

    const res = await fetch(remoteUrl, init);
    const data = await res.json();
    return NextResponse.json(data, { status: res.status });
  } catch (e: any) {
    // SECURITY (review L10): do not reflect the upstream body or the raw
    // exception message to the caller — it leaks internal reachability /
    // connection-error detail useful for enumeration. Log server-side only.
    console.error(`[Network Proxy] ${method} ${nodeId} failed:`, e?.message);
    return NextResponse.json({ error: 'Upstream request failed' }, { status: 502 });
  }
}

export async function GET(request: NextRequest, ctx: Params) {
  return proxyRequest(request, ctx, 'GET');
}

export async function POST(request: NextRequest, ctx: Params) {
  return proxyRequest(request, ctx, 'POST');
}
