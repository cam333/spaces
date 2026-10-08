import { NextRequest } from 'next/server';
import * as identity from '@/lib/network/api/identity';

export async function GET() {
  return identity.GET();
}

export async function PUT(req: NextRequest) {
  return identity.PUT(req);
}
