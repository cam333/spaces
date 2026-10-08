// tests/proto/agent-control.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import { encodePrompt, PROMPT_STALL_MS } from '../../bin/lib/runtime/agent-control.js';

describe('encodePrompt', () => {
  it('appends a carriage return in plain mode', () => {
    expect(encodePrompt('hello', { bracketedPaste: false })).toBe('hello\r');
  });

  it('wraps in bracketed-paste markers when asked', () => {
    expect(encodePrompt('hello', { bracketedPaste: true })).toBe('\x1b[200~hello\x1b[201~\r');
  });

  it('strips a trailing newline so the prompt is submitted once', () => {
    expect(encodePrompt('hello\n', { bracketedPaste: false })).toBe('hello\r');
    expect(encodePrompt('hello\r\n', { bracketedPaste: false })).toBe('hello\r');
  });

  it('preserves interior newlines', () => {
    expect(encodePrompt('a\nb', { bracketedPaste: true })).toBe('\x1b[200~a\nb\x1b[201~\r');
  });

  // A literal end-of-paste marker inside the caller's text would otherwise
  // close the bracketed-paste region early, dumping the remainder as live
  // keystrokes into the agent's TTY instead of pasted text.
  it('strips a literal end-of-paste marker from the body before wrapping in bracketed mode', () => {
    const malicious = 'hello\x1b[201~ rm -rf /\x1b[201~ world';
    const encoded = encodePrompt(malicious, { bracketedPaste: true });
    expect(encoded).toBe('\x1b[200~hello rm -rf / world\x1b[201~\r');
    // Exactly one end marker survives — the one this function adds itself.
    expect(encoded.split('\x1b[201~').length - 1).toBe(1);
  });

  it('exports a stall budget', () => {
    expect(PROMPT_STALL_MS).toBe(5000);
  });
});

let dir: string, dbPath: string, wsId: string;
let runtime: any;

// Windows cannot remove a directory containing a file with an open handle.
// createSessionStore() keeps its better-sqlite3 connection open for the
// runtime's lifetime by design (see session-store.js) and close() is
// terminal, so it can't be closed mid-dispatch — track the runtime created
// in each test and close its store here, before rmSync, the same way
// session-control.test.ts and workspace-control.test.ts do.
let runtimes: any[] = [];

beforeEach(() => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-ac-'));
  dbPath = path.join(dir, 'spaces.db');
  const db = new Database(dbPath);
  db.exec(`
    CREATE TABLE workspaces (
      id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT
    );
    CREATE TABLE panes (
      id TEXT PRIMARY KEY, title TEXT, color TEXT, cwd TEXT,
      agent_type TEXT DEFAULT 'shell', sort_order INTEGER DEFAULT 0,
      workspace_id INTEGER REFERENCES workspaces(id) ON DELETE CASCADE
    );
  `);
  db.close();
  runtime = createRuntime({ nodeId: 'test', spacesDbPath: dbPath });
  runtimes.push(runtime);
  wsId = (runtime.dispatch('workspace.create', { name: 'w' }) as any).result.workspaceId;
});

afterEach(() => {
  for (const rt of runtimes.splice(0)) {
    try { rt.sessionStore.close(); } catch { /* already closed */ }
  }
  fs.rmSync(dir, { recursive: true, force: true });
});

const settle = (ms = 500) => new Promise(r => setTimeout(r, ms));
const newPane = () =>
  (runtime.dispatch('pane.create', { workspaceId: wsId, agentType: 'shell', cwd: process.cwd() }) as any).result.paneId;

describe('agent.start', () => {
  it('types a resume command into a live pane', async () => {
    const paneId = newPane();
    await settle();
    const res = runtime.dispatch('agent.start', { paneId, agentType: 'claude', agentSession: 'abc' }) as any;
    expect(res.result).toMatchObject({ ok: true, launchCommand: 'claude --resume abc' });
    runtime.dispatch('pane.close', { paneId });
    await settle();
  }, 15000);

  it('rejects an agent type with no command', async () => {
    const paneId = newPane();
    await settle();
    expect((runtime.dispatch('agent.start', { paneId, agentType: 'shell' }) as any).error.code).toBe('bad_request');
    runtime.dispatch('pane.close', { paneId });
    await settle();
  }, 15000);

  it('rejects a missing pane and a blank agentType', () => {
    expect((runtime.dispatch('agent.start', { paneId: 'ghost', agentType: 'claude' }) as any).error.code).toBe('not_found');
    expect((runtime.dispatch('agent.start', { agentType: 'claude' }) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('agent.start', { paneId: 'x' }) as any).error.code).toBe('bad_request');
  });
});

describe('agent.prompt', () => {
  it('writes a prompt into a live pane and it reaches the shell', async () => {
    const paneId = newPane();
    await settle();
    const marker = `PROMPTED_${Date.now()}`;
    const res = runtime.dispatch('agent.prompt', { paneId, text: `echo ${marker}` }) as any;
    expect(res.result.ok).toBe(true);
    // The screen is the proof the bytes reached the pty.
    const deadline = Date.now() + 8000;
    let seen = false;
    while (Date.now() < deadline && !seen) {
      await settle(150);
      seen = runtime.getScreen(paneId)?.visibleGrid().join('\n').includes(marker) ?? false;
    }
    expect(seen).toBe(true);
    runtime.dispatch('pane.close', { paneId });
    await settle();
  }, 20000);

  it('rejects a non-string text, a blank paneId, and an unknown pane', () => {
    expect((runtime.dispatch('agent.prompt', { paneId: 'x', text: 42 }) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('agent.prompt', { text: 'hi' }) as any).error.code).toBe('bad_request');
    expect((runtime.dispatch('agent.prompt', { paneId: 'ghost', text: 'hi' }) as any).error.code).toBe('not_found');
  });

  it('accepts an empty string as a bare submit', async () => {
    const paneId = newPane();
    await settle();
    expect((runtime.dispatch('agent.prompt', { paneId, text: '' }) as any).result.ok).toBe(true);
    runtime.dispatch('pane.close', { paneId });
    await settle();
  }, 15000);
});
