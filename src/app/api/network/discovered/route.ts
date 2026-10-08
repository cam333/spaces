import * as discovered from '@/lib/network/api/discovered';

export async function GET(req: Request) {
  return discovered.GET(req);
}
