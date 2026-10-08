import * as analytics from '@/lib/admin/api/analytics';

export async function GET(req: Request) {
  return analytics.GET(req);
}
