import * as logout from '@/lib/auth/api/logout';

export async function POST(request: Request) {
  return logout.POST(request);
}
