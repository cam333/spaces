import { NextRequest } from 'next/server';
import * as keys from '@/lib/network/api/keys';

export async function GET(req: NextRequest) {
  return keys.GET(req);
}

export async function POST(req: NextRequest) {
  return keys.POST(req);
}
