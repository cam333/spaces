import { NextRequest } from 'next/server';
import * as nodes from '@/lib/network/api/nodes';

export async function GET(req: NextRequest) {
  return nodes.GET(req);
}

export async function POST(req: NextRequest) {
  return nodes.POST(req);
}
