import { NextRequest } from 'next/server';
import { getAuthUser } from '@/lib/auth';
import { sseManager } from '@/lib/events/sse';
import { initWatcher } from '@/lib/sync/watcher';
import { initDbChangeWatcher } from '@/lib/sync/db-change-watcher';

export async function GET(request: NextRequest) {
  const user = getAuthUser(request);

  // Initialize the watchers on first SSE connection. A subscriber that only
  // wants to hear about spaces and panes (`?topics=spaces`, the terminal
  // page) must not start session-file indexing as a side effect.
  if (request.nextUrl.searchParams.get('topics') !== 'spaces') initWatcher();
  initDbChangeWatcher();

  const stream = new ReadableStream({
    start(controller) {
      const id = crypto.randomUUID();
      sseManager.addClient(id, controller);

      // Send initial connection event
      const encoder = new TextEncoder();
      controller.enqueue(encoder.encode(`event: connected\ndata: ${JSON.stringify({ clientId: id, user })}\n\n`));

      // Keep-alive ping every 30s
      const keepAlive = setInterval(() => {
        try {
          controller.enqueue(encoder.encode(`: keepalive\n\n`));
        } catch {
          clearInterval(keepAlive);
          sseManager.removeClient(id);
        }
      }, 30000);
    },
  });

  return new Response(stream, {
    headers: {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection': 'keep-alive',
    },
  });
}
