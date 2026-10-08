import * as totpStatus from '@/lib/auth/api/totpStatus';

export async function GET(req: Request) {
  return totpStatus.GET(req);
}
