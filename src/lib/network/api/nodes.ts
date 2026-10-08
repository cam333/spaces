import { NextRequest, NextResponse } from 'next/server';
import { listNodes, addNode, updateNodeStatus, getNode } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import { rejectPrivateUrl } from '../url-guard';
import type { HandshakeResponse } from '../../../types/network';

export async function GET(request: NextRequest) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const nodes = listNodes();
  return NextResponse.json(nodes);
}

export async function POST(request: NextRequest) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const { url, apiKey, name } = await request.json() as any;

  if (!url || !apiKey) {
    return NextResponse.json({ error: 'URL and API key are required' }, { status: 400 });
  }
  if (typeof url !== 'string') {
    return NextResponse.json({ error: 'URL must be a string' }, { status: 400 });
  }

  const baseUrl = url.replace(/\/+$/, '');
  const reject = rejectPrivateUrl(baseUrl);
  if (reject) return NextResponse.json({ error: reject }, { status: 400 });

  let handshake: HandshakeResponse;
  try {
    const handshakeUrl = `${baseUrl}/api/network/handshake/`;
    const res = await fetch(handshakeUrl, {
      headers: { 'Authorization': `Bearer ${apiKey}` },
      signal: AbortSignal.timeout(10000),
    });

    if (!res.ok) {
      // Don't echo the upstream response body — it may contain sensitive
      // content (admin pages, cloud metadata) that the attacker probed for.
      return NextResponse.json(
        { error: `Handshake failed with status ${res.status}` },
        { status: 400 }
      );
    }

    handshake = await res.json() as any;
  } catch (e: any) {
    return NextResponse.json(
      { error: `Cannot reach node: ${e.message}` },
      { status: 400 }
    );
  }

  addNode({
    id: handshake.nodeId,
    name: name || handshake.nodeName,
    url: baseUrl,
    apiKey,
  });

  updateNodeStatus(handshake.nodeId, 'online', {
    sessionCount: handshake.sessionCount,
    version: handshake.version,
    name: name || handshake.nodeName,
  });

  const updated = getNode(handshake.nodeId);
  return NextResponse.json(updated, { status: 201 });
}
