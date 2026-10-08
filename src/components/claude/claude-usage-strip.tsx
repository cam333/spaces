'use client';

import { useEffect, useRef, useState } from 'react';
import { Zap, LogIn } from 'lucide-react';
import { useClaudeUsage } from '@/hooks/use-claude-usage';
import type { RateLimit } from '@/types/claude';

interface Props {
  enabled: boolean;
}

function utilizationColor(pct: number | null): string {
  if (pct == null) return '#52525b'; // zinc-600 — unknown
  if (pct <= 70) return '#22c55e';    // green-500
  if (pct <= 90) return '#f59e0b';    // amber-500
  return '#ef4444';                    // red-500
}

function formatReset(resetsAt: string | null): string {
  if (!resetsAt) return '';
  const ms = new Date(resetsAt).getTime() - Date.now();
  if (ms <= 0) return 'now';
  const h = Math.floor(ms / 3_600_000);
  const m = Math.floor((ms % 3_600_000) / 60_000);
  if (h >= 24) {
    const d = Math.floor(h / 24);
    const rh = h - d * 24;
    return `${d}d ${rh}h`;
  }
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function Bar({ label, limit }: { label: string; limit: RateLimit | null | undefined }) {
  if (!limit || limit.utilization == null) return null;
  const pct = Math.min(100, Math.max(0, limit.utilization));
  const color = utilizationColor(pct);
  const reset = formatReset(limit.resets_at);
  return (
    <div
      className="flex items-center gap-1.5 text-[10px]"
      title={reset ? `${label}: ${Math.floor(pct)}% used · resets in ${reset}` : `${label}: ${Math.floor(pct)}% used`}
    >
      <span className="text-zinc-400 font-medium">{label}</span>
      <div className="w-12 h-1 bg-zinc-800 rounded-full overflow-hidden">
        <div
          className="h-full transition-all"
          style={{ width: `${pct}%`, backgroundColor: color }}
        />
      </div>
      <span className="text-zinc-500 tabular-nums w-7 text-right">{Math.floor(pct)}%</span>
    </div>
  );
}

export function ClaudeUsageStrip({ enabled }: Props) {
  const response = useClaudeUsage(enabled);
  const [expanded, setExpanded] = useState(false);
  const popoverRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!expanded) return;
    function handleClick(e: MouseEvent) {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        setExpanded(false);
      }
    }
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [expanded]);

  if (!enabled || !response) return null;

  if (response.status === 'unauthenticated' || response.status === 'expired') {
    return (
      <div
        className="flex items-center gap-1.5 px-2 py-1 text-[10px] text-zinc-500 border border-zinc-800 rounded-md"
        title={response.status === 'expired' ? 'OAuth token expired — run any Claude Code command to refresh' : 'No Claude.ai credentials found'}
      >
        <LogIn className="w-3 h-3" />
        sign in via Claude Code
      </div>
    );
  }

  if (response.status === 'upstream_error') {
    return null; // Silent — per spec.
  }

  const { data } = response;
  const extra = data.extra_usage;
  const hasOverage = !!(extra?.is_enabled && extra?.used_credits && extra.used_credits > 0);

  return (
    <div className="relative">
      <button
        onClick={() => setExpanded(v => !v)}
        className="flex items-center gap-2.5 px-2.5 py-1 border border-zinc-800 rounded-md hover:border-zinc-700 transition-colors"
        title="Click for full usage detail"
      >
        <Zap className="w-3 h-3 text-amber-400" />
        <Bar label="5h" limit={data.five_hour} />
        <Bar label="7d" limit={data.seven_day} />
        <Bar label="Opus" limit={data.seven_day_opus} />
        <Bar label="Sonnet" limit={data.seven_day_sonnet} />
        {hasOverage && extra?.used_credits != null && (
          <span className="text-[10px] text-amber-400 border-l border-zinc-800 pl-2">
            +${extra.used_credits.toFixed(2)} overage
          </span>
        )}
      </button>

      {expanded && (
        <div
          ref={popoverRef}
          className="absolute top-full right-0 mt-1.5 z-50 w-80 p-3 bg-zinc-900 border border-zinc-700 rounded-md shadow-xl"
        >
          <div className="text-[10px] uppercase tracking-wider text-zinc-500 font-medium mb-2">
            Plan Usage
          </div>
          <div className="space-y-2">
            <ExpandedRow label="5-hour window" limit={data.five_hour} />
            <ExpandedRow label="7-day (overall)" limit={data.seven_day} />
            <ExpandedRow label="7-day · Opus" limit={data.seven_day_opus} />
            <ExpandedRow label="7-day · Sonnet" limit={data.seven_day_sonnet} />
            <ExpandedRow label="7-day · OAuth apps" limit={data.seven_day_oauth_apps} />
          </div>
          {extra?.is_enabled && (
            <div className="mt-3 pt-3 border-t border-zinc-800">
              <div className="text-[10px] uppercase tracking-wider text-zinc-500 font-medium mb-1.5">
                Overage credits
              </div>
              <div className="text-xs text-zinc-300">
                {extra.used_credits != null ? `$${extra.used_credits.toFixed(2)}` : '—'}
                {extra.monthly_limit != null && (
                  <span className="text-zinc-500"> / ${extra.monthly_limit.toFixed(2)} monthly</span>
                )}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function ExpandedRow({ label, limit }: { label: string; limit: RateLimit | null | undefined }) {
  if (!limit || limit.utilization == null) {
    return (
      <div className="flex items-center justify-between text-xs">
        <span className="text-zinc-400">{label}</span>
        <span className="text-zinc-600">—</span>
      </div>
    );
  }
  const pct = Math.min(100, Math.max(0, limit.utilization));
  const color = utilizationColor(pct);
  const reset = formatReset(limit.resets_at);
  return (
    <div>
      <div className="flex items-center justify-between text-xs mb-1">
        <span className="text-zinc-300">{label}</span>
        <span className="text-zinc-500 tabular-nums">
          {Math.floor(pct)}%{reset && <span className="ml-1.5 text-zinc-600">· {reset}</span>}
        </span>
      </div>
      <div className="h-1.5 bg-zinc-800 rounded-full overflow-hidden">
        <div
          className="h-full transition-all"
          style={{ width: `${pct}%`, backgroundColor: color }}
        />
      </div>
    </div>
  );
}
