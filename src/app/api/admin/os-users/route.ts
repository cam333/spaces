import * as osUsers from '@/lib/admin/api/osUsers';

export async function GET(req: Request) {
  return osUsers.GET(req);
}
