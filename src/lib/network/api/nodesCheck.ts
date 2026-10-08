import { NextResponse } from 'next/server';
import { listNodes, getNodeApiKey, updateNodeStatus } from '../db';
import { requireAdmin } from '@/lib/auth/guard';

export async function POST(request: Request) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const nodes = listNodes();
  const results: { nodeId: string; status: string; error?: string }[] = [];

  await Promise.allSettled(nodes.map(async (node) => {
    const apiKey = getNodeApiKey(node.id);
    if (!apiKey) {
      updateNodeStatus(node.id, 'error', { lastError: 'No API key' });
      results.push({ nodeId: node.id, status: 'error', error: 'No API key' });
      return;
    }

    try {
      const res = await fetch(`${node.url}/api/network/health`, {
        headers: { 'Authorization': `Bearer ${apiKey}` },
        signal: AbortSignal.timeout(10000),
      });

      if (res.ok) {
        const data: any = await res.json();
        // Also fetch session count from the remote handshake endpoint
        let sessionCount: number | undefined;
        try {
          const hsRes = await fetch(`${node.url}/api/network/handshake`, {
            headers: { 'Authorization': `Bearer ${apiKey}` },
            signal: AbortSignal.timeout(10000),
          });
          if (hsRes.ok) {
            const hsData: any = await hsRes.json();
            sessionCount = hsData.sessionCount;
          }
        } catch {}
        updateNodeStatus(node.id, 'online', {
          version: data.version,
          lastError: null,
          sessionCount,
        });
        results.push({ nodeId: node.id, status: 'online' });
      } else {
        const text = await res.text();
        updateNodeStatus(node.id, 'error', { lastError: `HTTP ${res.status}: ${text}` });
        results.push({ nodeId: node.id, status: 'error', error: `HTTP ${res.status}` });
      }
    } catch (e: any) {
      updateNodeStatus(node.id, 'offline', { lastError: e.message });
      results.push({ nodeId: node.id, status: 'offline', error: e.message });
    }
  }));

  return NextResponse.json({ results });
}
