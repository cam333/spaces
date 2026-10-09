'use strict';

/**
 * How many agents are live in this account's server on `port`, asked over
 * the session-control protocol with the owner's secret; null when the
 * server cannot be asked (not answering, refusing, too slow).
 */
function liveAgents(port, secret, { timeoutMs = 5000 } = {}) {
  const WebSocket = require('ws');
  const { signDesktop } = require('./desktop-auth');
  return new Promise((resolve) => {
    let ws;
    const done = (value) => {
      clearTimeout(timer);
      try { ws.terminate(); } catch { /* already gone */ }
      resolve(value);
    };
    const timer = setTimeout(() => done(null), timeoutMs);
    try {
      ws = new WebSocket(`ws://127.0.0.1:${port}/proto`, {
        headers: {
          Authorization: `Bearer ${signDesktop(secret, 'session', 60)}`,
          Host: `localhost:${port}`,
        },
      });
    } catch {
      done(null);
      return;
    }
    ws.on('open', () => ws.send(JSON.stringify({ type: 'hello', protocolVersion: 1, client: 'spaces-status' })));
    ws.on('message', (data) => {
      let m;
      try { m = JSON.parse(String(data)); } catch { return; }
      if (m.type === 'hello-ok') {
        ws.send(JSON.stringify({ id: '1', method: 'session.snapshot', params: {} }));
      } else if (m.id === '1') {
        const panes = m.result && Array.isArray(m.result.panes) ? m.result.panes : null;
        done(panes ? panes.filter((p) => p.live).length : null);
      }
    });
    ws.on('error', () => done(null));
    ws.on('close', () => done(null));
  });
}

module.exports = { liveAgents };
