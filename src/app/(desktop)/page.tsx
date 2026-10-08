import { cookies } from 'next/headers';
import { redirect } from 'next/navigation';
import DashboardClient from './dashboard-client';
import { IS_MULTIUSER } from '@/lib/flags';

export const dynamic = 'force-dynamic';

export default async function DashboardPage() {
  if (IS_MULTIUSER) {
    const cookieStore = await cookies();
    if (!cookieStore.get('spaces-session')) {
      redirect('/login');
    }
  }

  return <DashboardClient />;
}
