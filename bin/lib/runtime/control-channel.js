'use strict';

const { PROTOCOL_VERSION, schemas } = require('./protocol');

const HELLO_TIMEOUT_MS = 5000;

// Wait outcomes mapped onto the protocol's existing error codes. The precise
// reason survives in the message, prefixed with the original code word.
const WAIT_CODE_MAP = {
  bad_request: 'bad_request',
  not_found: 'not_found',
  timeout: 'conflict',
  agent_prompt_stalled: 'conflict',
};

/**
 * Attaches the control-channel protocol to a single WebSocket connection.
 *
 * Handshake: the first client frame must validate as `Hello` within
 * HELLO_TIMEOUT_MS or the socket is closed with a `bad_request` error.
 * A protocol version mismatch closes with `version_mismatch`. After hello,
 * `Request` frames are routed to `runtime.dispatch`, plus the channel-level
 * `events.subscribe` method (backlog reply, then live push of every future
 * event). Unauthenticated sockets are rejected immediately.
 */
/**
 * @param {object} opts
 * @param {boolean} opts.authenticated
 * @param {(method: string, params: object) => {allowed: boolean, reason?: string}} [opts.policy]
 *   Consulted before **every** request when present. A federated peer is given
 *   one; a local client is not, and the difference is the whole point — a
 *   credential stolen from a peer must be worth less than one taken from this
 *   machine. Applied at the single point every method passes through, rather
 *   than per method, so a capability added later is covered without anyone
 *   remembering to cover it.
 */
function attachControlChannel(ws, runtime, { authenticated, policy = null }) {
  // Registered before any early return, including the unauthenticated path
  // below: an 'error' event on an EventEmitter with zero listeners is fatal
  // in Node (it throws and takes down the whole process). This socket is
  // reachable pre-auth — any TCP client that completes the /proto WS
  // handshake gets one — so it must never go unlistened.
  ws.on('error', (err) => {
    console.error(`[Proto] Socket error: ${err.message}`);
  });

  if (!authenticated) {
    ws.send(JSON.stringify({ id: 'hello', error: { code: 'unauthorized', message: 'authentication required' } }));
    ws.close();
    return;
  }

  // Liveness: paired with the ping/pong sweep in terminal-server.js's
  // setupProtoWss (mirrors the legacy /ws heartbeat in handleConnection) —
  // that sweep terminates any socket that doesn't answer a ping within one
  // 30s tick, so a silently-dead peer doesn't hold its hub.subscribe
  // listener (and the memory it references) forever.
  ws.isAlive = true;
  ws.on('pong', () => { ws.isAlive = true; });

  let helloDone = false;
  let unsubscribe = null;

  const helloTimer = setTimeout(() => {
    if (!helloDone) {
      ws.send(JSON.stringify({ id: 'hello', error: { code: 'bad_request', message: 'hello timeout' } }));
      ws.close();
    }
  }, HELLO_TIMEOUT_MS);
  if (helloTimer.unref) helloTimer.unref();

  ws.on('message', (raw) => {
    let msg;
    try { msg = JSON.parse(raw.toString()); } catch {
      ws.send(JSON.stringify({ id: 'unknown', error: { code: 'bad_request', message: 'invalid json' } }));
      return;
    }

    // Normalize a missing/null `params` to {} up front so neither schema
    // validation nor runtime.dispatch (whose `= {}` default only covers
    // `undefined`, not `null`) ever sees `params: null` and crashes on
    // property access.
    if (msg && msg.params === null) msg.params = {};

    if (!helloDone) {
      const hello = schemas.Hello.safeParse(msg);
      if (!hello.success) {
        ws.send(JSON.stringify({ id: 'hello', error: { code: 'bad_request', message: 'expected hello' } }));
        return;
      }
      if (hello.data.protocolVersion !== PROTOCOL_VERSION) {
        ws.send(JSON.stringify({
          id: 'hello',
          error: { code: 'version_mismatch', message: `runtime speaks protocol ${PROTOCOL_VERSION}` },
        }));
        ws.close();
        return;
      }
      helloDone = true;
      clearTimeout(helloTimer);
      // node.info is a cheap in-memory lookup (no DB access), unlike
      // snapshot() — avoids building a full snapshot just to read nodeId.
      const nodeInfo = runtime.dispatch('node.info', {}).result;
      ws.send(JSON.stringify({
        type: 'hello-ok', protocolVersion: PROTOCOL_VERSION,
        nodeId: nodeInfo.nodeId, seq: nodeInfo.seq,
      }));
      return;
    }

    const req = schemas.Request.safeParse(msg);
    if (!req.success) {
      ws.send(JSON.stringify({ id: String(msg?.id ?? 'unknown'), error: { code: 'bad_request', message: 'invalid request' } }));
      return;
    }
    const { id, method, params = {} } = req.data;

    // Before anything else routes: the gate sits above `events.subscribe` and
    // the async methods too, not only above `runtime.dispatch`, because those
    // branches return early and would otherwise bypass it entirely.
    if (policy) {
      const verdict = policy(method, params);
      if (!verdict.allowed) {
        ws.send(JSON.stringify({ id, error: { code: 'unauthorized', message: verdict.reason } }));
        return;
      }
    }

    if (method === 'events.subscribe') {
      const afterSeq = Number(params.afterSeq ?? 0);
      // A non-numeric/garbage afterSeq (object, array, non-numeric string,
      // NaN, a float, or negative) previously coerced to NaN via Number().
      // eventsAfter(NaN) then returns an empty backlog with gap:false —
      // silently telling the client it's fully caught up when it isn't
      // (M3). Reject it outright instead.
      if (!Number.isInteger(afterSeq) || afterSeq < 0) {
        ws.send(JSON.stringify({ id, error: { code: 'bad_request', message: 'afterSeq must be a non-negative integer' } }));
        return;
      }
      const { gap, events } = runtime.hub.eventsAfter(afterSeq);
      ws.send(JSON.stringify({ id, result: { gap, events } }));
      if (unsubscribe) unsubscribe();
      unsubscribe = runtime.hub.subscribe(event => {
        if (ws.readyState === ws.OPEN) ws.send(JSON.stringify({ event }));
      });
      return;
    }

    if (method === 'detect.update') {
      runtime.refreshDetectionManifests()
        .then(result => { if (ws.readyState === ws.OPEN) ws.send(JSON.stringify({ id, result })); })
        .catch(error => {
          if (ws.readyState === ws.OPEN) {
            ws.send(JSON.stringify({ id, error: { code: 'internal', message: String(error && error.message || error) } }));
          }
        });
      return;
    }

    // Crosses the network to another machine, so it cannot go through the
    // synchronous `dispatch`. A peer that is offline or was never paired comes
    // back as an error on this request rather than stalling the channel.
    if (method === 'net.snapshot' || method === 'net.request') {
      const run =
        method === 'net.snapshot' ? runtime.peerSnapshot(params) : runtime.peerRequest(params);
      run
        .then((result) => {
          if (ws.readyState === ws.OPEN) ws.send(JSON.stringify({ id, result }));
        })
        .catch((error) => {
          if (ws.readyState === ws.OPEN) {
            ws.send(JSON.stringify({
              id,
              error: { code: 'not_found', message: String((error && error.message) || error) },
            }));
          }
        });
      return;
    }

    if (method === 'agent.wait' || method === 'agent.prompt-wait') {
      const run = method === 'agent.wait'
        ? runtime.awaitAgentState(params)
        : runtime.promptAndWait(params);
      run
        .then(outcome => {
          if (ws.readyState !== ws.OPEN) return;
          if (outcome.ok) ws.send(JSON.stringify({ id, result: outcome }));
          else ws.send(JSON.stringify({
            id,
            error: {
              code: WAIT_CODE_MAP[outcome.code] || 'internal',
              message: `${outcome.code}: ${outcome.error}`,
            },
          }));
        })
        .catch(error => {
          if (ws.readyState === ws.OPEN) {
            ws.send(JSON.stringify({ id, error: { code: 'internal', message: String((error && error.message) || error) } }));
          }
        });
      return;
    }

    const outcome = runtime.dispatch(method, params);
    ws.send(JSON.stringify({ id, ...outcome }));
  });

  ws.on('close', () => {
    clearTimeout(helloTimer);
    if (unsubscribe) unsubscribe();
  });
}

module.exports = { attachControlChannel };
