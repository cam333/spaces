export type PaneUpFrame = { t: 'i'; data: string } | { t: 'r'; cols: number; rows: number };

export function buildPaneSocketUrl(opts: {
  origin: string; basePath: string; paneId: string;
  mode: 'observe' | 'control'; takeover?: boolean; token?: string;
}): string {
  const url = new URL(opts.origin);
  const proto = url.protocol === 'https:' ? 'wss:' : 'ws:';
  const params = new URLSearchParams({ mode: opts.mode });
  if (opts.takeover) params.set('takeover', '1');
  if (opts.token) params.set('terminalToken', opts.token);
  return `${proto}//${url.host}${opts.basePath}/proto/pane/${encodeURIComponent(opts.paneId)}?${params}`;
}

export function encodeUpFrame(frame: PaneUpFrame): string {
  return JSON.stringify(frame);
}

/**
 * Attach to a pane's data socket. Binary frames are terminal bytes
 * (replay first, then live). Text frames are JSON control events
 * (replay-start/replay-end/error).
 */
export function openPaneSocket(opts: {
  origin: string; basePath: string; paneId: string;
  mode: 'observe' | 'control'; takeover?: boolean; token?: string;
  onBytes: (bytes: Uint8Array) => void;
  onEvent: (evt: { type: string; code?: string; message?: string }) => void;
}): { ws: WebSocket; sendInput: (data: string) => void; sendResize: (cols: number, rows: number) => void } {
  const ws = new WebSocket(buildPaneSocketUrl(opts));
  ws.binaryType = 'arraybuffer';
  ws.onmessage = (e) => {
    if (e.data instanceof ArrayBuffer) opts.onBytes(new Uint8Array(e.data));
    else { try { opts.onEvent(JSON.parse(e.data)); } catch { /* ignore malformed */ } }
  };
  return {
    ws,
    sendInput: (data) => { if (ws.readyState === WebSocket.OPEN) ws.send(encodeUpFrame({ t: 'i', data })); },
    sendResize: (cols, rows) => { if (ws.readyState === WebSocket.OPEN) ws.send(encodeUpFrame({ t: 'r', cols, rows })); },
  };
}
