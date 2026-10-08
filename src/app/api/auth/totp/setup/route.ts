import * as totpSetup from '@/lib/auth/api/totpSetup';

export async function POST(req: Request) {
  return totpSetup.POST(req);
}
