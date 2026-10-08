import * as nodesCheck from '@/lib/network/api/nodesCheck';

export async function POST(req: Request) {
  return nodesCheck.POST(req);
}
