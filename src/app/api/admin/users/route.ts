import * as users from '@/lib/admin/api/users';

export async function GET(req: Request) {
  return users.GET(req);
}

export async function POST(req: Request) {
  return users.POST(req);
}
