import { NextRequest } from 'next/server';
import * as health from '@/lib/network/api/health';

export async function GET(req: NextRequest) {
  return health.GET(req);
}
