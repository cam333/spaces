import {
  getLoginEvents,
  getTerminalSessions,
  getActiveSessionCount,
  getDailyLoginCounts,
  getDailySessionMinutes,
  getUserSessionSummary,
} from '../db';
import { requireAdmin } from '@/lib/auth/guard';

export async function GET(request: Request) {
  const denied = requireAdmin(request);
  if (denied) return denied;

  const url = new URL(request.url);
  const days = Math.min(Math.max(parseInt(url.searchParams.get('days') || '30', 10), 1), 365);

  try {
    const activeSessions = getActiveSessionCount();
    const recentLogins = getLoginEvents(days);
    const sessions = getTerminalSessions(days);
    const dailyLogins = getDailyLoginCounts(days);
    const dailySessionTime = getDailySessionMinutes(days);
    const userSummary = getUserSessionSummary(days);

    const totalLogins = recentLogins.length;
    const totalSessionMinutes = Math.round(
      sessions.reduce((sum: number, s: any) => sum + (s.duration_seconds || 0), 0) / 60
    );

    return Response.json({
      activeSessions,
      totalLogins,
      totalSessionMinutes,
      recentLogins: recentLogins.slice(0, 100),
      dailyLogins,
      dailySessionTime,
      userSummary,
    });
  } catch (err: any) {
    console.error('[Admin Analytics]', err);
    return Response.json({ error: 'Internal server error' }, { status: 500 });
  }
}
