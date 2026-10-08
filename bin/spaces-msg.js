#!/usr/bin/env node

const http = require('http');

const API_URL = process.env.SPACES_API_URL || 'http://localhost:3457';
// The server answers only its owner (see lib/desktop-auth.js). This runs as
// the owner, so it can sign a short-lived token with the owner's secret; the
// secret itself never goes on the wire, and nothing goes off this machine.
const authHeaders = () => require('./lib/desktop-auth').clientAuthHeaders(API_URL);
const WORKSPACE_ID = process.env.SPACES_WORKSPACE_ID;
const PANE_ID = process.env.SPACES_PANE_ID;
const PANE_NAME = process.env.SPACES_PANE_NAME || 'Unknown';
const BASE_PATH = process.env.SPACES_BASE_PATH || '';

function request(method, path, body) {
  return new Promise((resolve, reject) => {
    const url = new URL(`${API_URL}${BASE_PATH}${path}`);
    const options = {
      hostname: url.hostname,
      port: url.port,
      path: url.pathname + url.search,
      method,
      headers: { 'Content-Type': 'application/json', ...authHeaders() },
    };
    const req = http.request(options, (res) => {
      let data = '';
      res.on('data', (chunk) => (data += chunk));
      res.on('end', () => {
        try { resolve(JSON.parse(data)); }
        catch { resolve(data); }
      });
    });
    req.on('error', reject);
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

async function main() {
  const [,, command, ...args] = process.argv;

  if (!WORKSPACE_ID && command !== 'help') {
    console.error('Error: SPACES_WORKSPACE_ID not set. Are you running inside a Spaces pane?');
    process.exit(1);
  }

  switch (command) {
    case 'post': {
      const toIdx = args.indexOf('--to');
      let targetName = null;
      let content;
      if (toIdx !== -1) {
        targetName = args[toIdx + 1];
        content = args.filter((_, i) => i !== toIdx && i !== toIdx + 1).join(' ');
      } else {
        content = args.join(' ');
      }
      const msg = await request('POST', `/api/workspaces/${WORKSPACE_ID}/messages/`, {
        content,
        senderName: PANE_NAME,
        paneId: PANE_ID,
        type: 'message',
        isAgent: true,
        ...(targetName && { metadata: { targetName } }),
      });
      console.log(`Posted: ${msg.content}`);
      break;
    }

    case 'request': {
      const content = args.join(' ');
      const msg = await request('POST', `/api/workspaces/${WORKSPACE_ID}/messages/`, {
        content,
        senderName: PANE_NAME,
        paneId: PANE_ID,
        type: 'request',
        isAgent: true,
      });
      console.log(`Request posted (awaiting approval): ${msg.content}`);
      break;
    }

    case 'read': {
      const sinceIdx = args.indexOf('--since');
      let since;
      if (sinceIdx !== -1) {
        const val = args[sinceIdx + 1];
        const match = val.match(/^(\d+)(m|h|d)$/);
        if (match) {
          const ms = { m: 60000, h: 3600000, d: 86400000 }[match[2]];
          since = new Date(Date.now() - parseInt(match[1]) * ms).toISOString();
        }
      }
      const sp = new URLSearchParams();
      if (since) sp.set('since', since);
      const messages = await request('GET', `/api/workspaces/${WORKSPACE_ID}/messages/?${sp}`);
      if (!Array.isArray(messages) || messages.length === 0) {
        console.log('No messages.');
      } else {
        for (const m of messages.reverse()) {
          const time = new Date(m.created).toLocaleTimeString();
          const status = m.status !== 'delivered' ? ` [${m.status}]` : '';
          console.log(`[${time}] ${m.senderName}: ${m.content}${status}`);
        }
      }
      break;
    }

    case 'context': {
      const [subCmd, key, ...rest] = args;
      if (subCmd === 'get') {
        if (key) {
          const entry = await request('GET', `/api/workspaces/${WORKSPACE_ID}/context/${encodeURIComponent(key)}/`);
          console.log(entry ? entry.value : `No context entry: ${key}`);
        } else {
          const entries = await request('GET', `/api/workspaces/${WORKSPACE_ID}/context/`);
          if (!Array.isArray(entries) || entries.length === 0) {
            console.log('No context entries.');
          } else {
            for (const e of entries) {
              console.log(`${e.key} = ${e.value} (by ${e.updatedBy})`);
            }
          }
        }
      } else if (subCmd === 'set' && key) {
        const value = rest.join(' ');
        await request('PUT', `/api/workspaces/${WORKSPACE_ID}/context/${encodeURIComponent(key)}/`, {
          value,
          updatedBy: PANE_NAME,
        });
        console.log(`Set ${key} = ${value}`);
      } else if (subCmd === 'delete' && key) {
        await request('DELETE', `/api/workspaces/${WORKSPACE_ID}/context/${encodeURIComponent(key)}/`);
        console.log(`Deleted ${key}`);
      } else {
        console.log('Usage: spaces-msg context get [key] | set <key> <value> | delete <key>');
      }
      break;
    }

    case 'agents': {
      const panes = await request('GET', `/api/panes/`);
      if (!Array.isArray(panes)) {
        console.log('Could not fetch panes.');
      } else {
        const inWorkspace = panes.filter(p => String(p.workspaceId) === WORKSPACE_ID);
        for (const p of inWorkspace) {
          const me = p.id === PANE_ID ? ' (you)' : '';
          console.log(`${p.title} [${p.agentType || 'shell'}]${me}`);
        }
      }
      break;
    }

    default:
      console.log(`spaces-msg — Workspace message bus CLI

Usage:
  spaces-msg post [--to <pane>] <message>    Post a message
  spaces-msg request <message>               Post a request (needs approval)
  spaces-msg read [--since 5m|1h|1d]         Read recent messages
  spaces-msg context get [key]               Read shared context
  spaces-msg context set <key> <value>       Write shared context
  spaces-msg context delete <key>            Delete context entry
  spaces-msg agents                          List active agents

Environment: SPACES_WORKSPACE_ID, SPACES_PANE_ID, SPACES_API_URL`);
  }
}

main().catch((err) => {
  console.error('Error:', err.message);
  process.exit(1);
});
