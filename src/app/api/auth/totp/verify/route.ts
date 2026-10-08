import * as totpVerify from '@/lib/auth/api/totpVerify';

export async function POST(req: Request) {
  return totpVerify.POST(req);
}
