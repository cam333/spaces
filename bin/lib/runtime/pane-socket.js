'use strict';

const { schemas } = require('./protocol');

/**
 * Attaches the pane-data-socket protocol to a single WebSocket connection.
 *
 * Attach sequence: text `{"type":"replay-start"}` -> one binary frame (the
 * serialized screen restore) -> text `{"type":"replay-end"}` -> live PTY
 * chunks as binary frames, thereafter.
 *
 * `mode: 'control'` claims the pane's single controller slot; a second
 * control attach is rejected with a `conflict` error unless `takeover` is
 * set, which demotes the previous controller. `mode: 'observe'` (default)
 * never claims control; its up-frames are rejected with `bad_request`.
 */
function attachPaneSocket(ws, runtime, { paneId, mode = 'observe', takeover = false, authenticated }) {
  // Registered before any early return: an 'error' event on an EventEmitter
  // with zero listeners is fatal in Node (it throws and takes down the whole
  // process). This socket is reachable pre-auth, so it must never go
  // unlistened (same reasoning as the /proto control channel).
  ws.on('error', (err) => {
    console.error(`[Proto] Pane socket error: ${err.message}`);
  });

  // Validates every down-frame against the PaneDownFrame schema (protocol.js)
  // before it goes out — safeParse rather than parse/throw, since this path
  // is reachable from network-triggered code and a schema mismatch here is a
  // server bug, not something that should ever crash the process.
  const sendJson = (obj) => {
    const parsed = schemas.PaneDownFrame.safeParse(obj);
    if (!parsed.success) {
      console.error('[Proto] Pane socket down-frame failed schema validation:', parsed.error.message);
    }
    if (ws.readyState === ws.OPEN) ws.send(JSON.stringify(obj));
  };

  if (!authenticated) { sendJson({ type: 'error', code: 'unauthorized', message: 'authentication required' }); ws.close(); return; }

  const screen = runtime.getScreen(paneId);
  if (!screen) { sendJson({ type: 'error', code: 'not_found', message: `no live pane ${paneId}` }); ws.close(); return; }

  // Liveness: paired with the ping/pong sweep in terminal-server.js's
  // setupProtoWss — pane sockets ride the same protoWss as the control
  // channel, so a silently-dead peer doesn't hold its data subscription (and
  // controller slot) forever.
  ws.isAlive = true;
  ws.on('pong', () => { ws.isAlive = true; });

  let isController = false;
  const kicked = () => {
    isController = false;
    sendJson({ type: 'error', code: 'conflict', message: 'taken over by another client' });
  };

  if (mode === 'control') {
    const claim = runtime.claimController(paneId, kicked, { takeover });
    if (!claim.ok) {
      sendJson({ type: 'error', code: claim.reason === 'not_found' ? 'not_found' : 'conflict', message: 'pane already controlled' });
      ws.close();
      return;
    }
    isController = true;
  }

  // Replay: current screen state (colors + scrollback) as one binary frame.
  sendJson({ type: 'replay-start' });
  ws.send(Buffer.from(screen.replayAnsi(), 'utf8'), { binary: true });
  sendJson({ type: 'replay-end' });

  const unsubscribe = runtime.subscribePaneData(
    paneId,
    (chunk) => { if (ws.readyState === ws.OPEN) ws.send(Buffer.from(chunk, 'utf8'), { binary: true }); },
    // Pane exited: tell this socket (observer or controller) so it never
    // sits OPEN forever with no terminal frame, and so a controller's next
    // input isn't accepted and silently dropped (I1).
    (exitCode) => { sendJson({ type: 'exit', exitCode }); ws.close(); },
  );

  ws.on('message', (raw, isBinary) => {
    if (isBinary) { sendJson({ type: 'error', code: 'bad_request', message: 'up-frames are json text' }); return; }
    let frame;
    try { frame = JSON.parse(raw.toString()); } catch { sendJson({ type: 'error', code: 'bad_request', message: 'invalid json' }); return; }
    const parsed = schemas.PaneUpFrame.safeParse(frame);
    if (!parsed.success) { sendJson({ type: 'error', code: 'bad_request', message: 'invalid frame' }); return; }
    if (!isController) { sendJson({ type: 'error', code: 'bad_request', message: 'observer cannot send input' }); return; }
    if (parsed.data.t === 'i') runtime.writePane(paneId, parsed.data.data);
    else runtime.resizePane(paneId, parsed.data.cols, parsed.data.rows);
  });

  ws.on('close', () => {
    unsubscribe();
    if (mode === 'control') runtime.releaseController(paneId, kicked);
  });
}

module.exports = { attachPaneSocket };
