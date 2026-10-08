import { NextRequest, NextResponse } from 'next/server';
import { getNode, updateNode, removeNode } from '../db';
import { requireAdmin } from '@/lib/auth/guard';
import { rejectPrivateUrl } from '../url-guard';

export async function PUT(request: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const { id } = await params;
  const data: any = await request.json();

  const node = getNode(id);
  if (!node) {
    return NextResponse.json({ error: 'Node not found' }, { status: 404 });
  }

  // SECURITY (review H5): the add-node path validates the URL with
  // rejectPrivateUrl, but the update path did not — letting an admin repoint a
  // node at 169.254.169.254 / loopback and turn the proxy into a credentialed
  // SSRF read. Validate any URL change here too.
  if (typeof data?.url === 'string') {
    const reject = rejectPrivateUrl(data.url.replace(/\/+$/, ''));
    if (reject) return NextResponse.json({ error: reject }, { status: 400 });
  }

  updateNode(id, data);
  const updated = getNode(id);
  return NextResponse.json(updated);
}

export async function DELETE(request: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const denied = requireAdmin(request);
  if (denied) return denied;
  const { id } = await params;

  const node = getNode(id);
  if (!node) {
    return NextResponse.json({ error: 'Node not found' }, { status: 404 });
  }

  removeNode(id);
  return NextResponse.json({ ok: true });
}
