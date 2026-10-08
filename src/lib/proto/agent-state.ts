export type DisplayState = 'blocked' | 'working' | 'done' | 'idle' | 'unknown';

export interface PaneStateLike {
  agentState: string;
  seen: boolean;
}

/** `done` is not a protocol state — it is an unseen completion. */
export function displayState(pane: any): DisplayState {
  // Gracefully handle null, undefined, non-objects, or missing fields.
  if (!pane || typeof pane !== 'object' || typeof pane.agentState !== 'string') {
    return 'unknown';
  }

  // Use falsy check (!pane.seen) to match server-side behavior in bin/lib/runtime/state.js.
  if (pane.agentState === 'idle' && !pane.seen) return 'done';
  if (pane.agentState === 'blocked') return 'blocked';
  if (pane.agentState === 'working') return 'working';
  if (pane.agentState === 'idle') return 'idle';
  return 'unknown';
}

export const STATE_PRIORITY: Record<DisplayState, number> = {
  blocked: 5, working: 4, done: 3, idle: 2, unknown: 1,
};

export function rollupState(panes: PaneStateLike[]): DisplayState {
  let best: DisplayState = 'unknown';
  for (const pane of panes) {
    // Skip entries that aren't valid objects.
    if (!pane || typeof pane !== 'object') continue;
    const s = displayState(pane);
    if (STATE_PRIORITY[s] > STATE_PRIORITY[best]) best = s;
  }
  return best;
}

export function stateLabel(state: DisplayState): string {
  switch (state) {
    case 'blocked': return 'Needs you';
    case 'working': return 'Working';
    case 'done': return 'Done';
    case 'idle': return 'Idle';
    default: return '—';
  }
}

export function stateTone(state: DisplayState): 'danger' | 'active' | 'success' | 'muted' {
  switch (state) {
    case 'blocked': return 'danger';
    case 'working': return 'active';
    case 'done': return 'success';
    default: return 'muted';
  }
}
