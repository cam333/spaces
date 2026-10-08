'use client';

import { useEffect, useRef, useState } from 'react';
import { api } from '@/lib/api';
import type { UsageResponse } from '@/types/claude';

const POLL_INTERVAL_MS = 60_000;
const TRANSIENT_TOLERANCE = 2; // consecutive bad polls before we clobber good data

export function useClaudeUsage(enabled: boolean): UsageResponse | null {
  const [response, setResponse] = useState<UsageResponse | null>(null);
  const badPollsRef = useRef(0);

  useEffect(() => {
    if (!enabled) {
      badPollsRef.current = 0;
      return;
    }

    let cancelled = false;

    async function load() {
      try {
        const res = await fetch(api('/api/claude/usage'));
        if (!res.ok || cancelled) return;
        const data: UsageResponse = await res.json();
        if (cancelled) return;

        const isTransient = data.status === 'upstream_error' || data.status === 'expired';
        if (isTransient) {
          badPollsRef.current += 1;
          setResponse(prev => {
            // If we already have a good response, keep it until the bad state persists.
            if (prev?.status === 'ok' && badPollsRef.current < TRANSIENT_TOLERANCE) {
              return prev;
            }
            return data;
          });
        } else {
          badPollsRef.current = 0;
          setResponse(data);
        }
      } catch {
        // Network error — treat as transient.
        if (cancelled) return;
        badPollsRef.current += 1;
        setResponse(prev => {
          if (prev?.status === 'ok' && badPollsRef.current < TRANSIENT_TOLERANCE) {
            return prev;
          }
          return { status: 'upstream_error' };
        });
      }
    }

    load();
    const timer = setInterval(load, POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [enabled]);

  return response;
}
