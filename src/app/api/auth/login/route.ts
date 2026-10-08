import * as login from '@/lib/auth/api/login';

export async function POST(req: Request) {
  return login.POST(req);
}
