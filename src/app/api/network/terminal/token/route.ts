import { NextRequest } from 'next/server';
import * as terminalToken from '@/lib/network/api/terminalToken';

export async function POST(req: NextRequest) {
  return terminalToken.POST(req);
}
