// tests/proto/pane-restart.test.ts
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';

// A server restart or reboot leaves every pane as a database row with no
// process behind it. pane.restart brings one back in place.

let dir: string, dbPath: string;
let runtime: any;

beforeEach(() => {
  process.env.SPACES_NO_DETECTION_UPDATE = '1';
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-restart-'));
  dbPath = path.join(dir, 'spaces.db');
  const db = new Database(dbPath);
  db.exec(`
    CREATE TABLE workspaces (
      id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT
    );
    CREATE TABLE panes (
      id TEXT PRIMARY KEY, title TEXT, color TEXT, cwd TEXT,
      agent_type TEXT DEFAULT 'shell', sort_order INTEGER DEFAULT 0,
      claude_session_id TEXT, custom_command TEXT,
      workspace_id INTEGER REFERENCES workspaces(id) ON DELETE CASCADE
    );
    INSERT INTO workspaces (name) VALUES ('dev');
  `);
  // A pane from before the restart: a row, no process.
  db.prepare("INSERT INTO panes (id, title, cwd, agent_type, workspace_id) VALUES ('stopped', 'shell', ?, 'shell', 1)")
    .run(process.cwd());
  db.close();
  runtime = createRuntime({ nodeId: 'test', spacesDbPath: dbPath });
});

afterEach(() => {
  try { runtime.sessionStore.close(); } catch { /* already closed */ }
  fs.rmSync(dir, { recursive: true, force: true });
});

const settle = (ms = 500) => new Promise(r => setTimeout(r, ms));
const paneIn = (id: string) => runtime.snapshot().panes.find((p: any) => p.id === id);

describe('pane.restart', () => {
  it('starts a stopped pane again under the same id, in the same space', async () => {
    expect(paneIn('stopped')).toMatchObject({ live: false, workspaceId: 'w1' });
    const seq = runtime.hub._nextSeq - 1;
    const res = runtime.dispatch('pane.restart', { paneId: 'stopped', cols: 80, rows: 24 }) as any;
    expect(res.result).toMatchObject({ paneId: 'stopped', workspaceId: 'w1', agentType: 'shell', resumed: false });
    expect(paneIn('stopped')).toMatchObject({ live: true, workspaceId: 'w1' });
    // Clients learn it is running from the same event a fresh spawn sends.
    const opened = runtime.hub.eventsAfter(seq).events.find((e: any) => e.type === 'pane.opened');
    expect(opened?.payload).toMatchObject({ paneId: 'stopped', workspaceId: 'w1' });
    runtime.dispatch('pane.close', { paneId: 'stopped' });
    await settle();
  }, 15000);

  it('refuses a pane that is already running', async () => {
    runtime.dispatch('pane.restart', { paneId: 'stopped' });
    await settle();
    expect((runtime.dispatch('pane.restart', { paneId: 'stopped' }) as any).error.code).toBe('bad_request');
    runtime.dispatch('pane.close', { paneId: 'stopped' });
    await settle();
  }, 15000);

  it('can start a pane again after its process exits', async () => {
    runtime.dispatch('pane.restart', { paneId: 'stopped' });
    await settle();
    runtime.dispatch('agent.prompt', { paneId: 'stopped', text: 'exit' });
    const deadline = Date.now() + 8000;
    while (Date.now() < deadline && paneIn('stopped')?.live) await settle(150);
    expect(paneIn('stopped')?.live).toBe(false);
    expect((runtime.dispatch('pane.restart', { paneId: 'stopped' }) as any).result.paneId).toBe('stopped');
    expect(paneIn('stopped')?.live).toBe(true);
    runtime.dispatch('pane.close', { paneId: 'stopped' });
    await settle();
  }, 20000);

  it('rejects an unknown pane and a missing paneId', () => {
    expect((runtime.dispatch('pane.restart', { paneId: 'ghost' }) as any).error.code).toBe('not_found');
    expect((runtime.dispatch('pane.restart', {}) as any).error.code).toBe('bad_request');
  });
});

describe('pane.restart launches the agent', () => {
  it('runs the pane\'s command again, not just a shell', async () => {
    const db = new Database(dbPath);
    const marker = `RESTARTED_${Date.now()}`;
    db.prepare("INSERT INTO panes (id, title, cwd, agent_type, custom_command, workspace_id) VALUES ('custom', 'x', ?, 'custom', ?, 1)")
      .run(process.cwd(), `echo ${marker}`);
    db.close();
    const res = runtime.dispatch('pane.restart', { paneId: 'custom' }) as any;
    expect(res.result.launchCommand).toBe(`echo ${marker}`);
    const deadline = Date.now() + 8000;
    let seen = false;
    while (Date.now() < deadline && !seen) {
      await settle(150);
      // The command echoed back and its output: the marker appears twice.
      seen = (runtime.getScreen('custom')?.visibleGrid().join('\n').split(marker).length ?? 0) > 2;
    }
    expect(seen).toBe(true);
    runtime.dispatch('pane.close', { paneId: 'custom' });
    await settle();
  }, 20000);

  it('resumes a Claude conversation only when its transcript exists', async () => {
    const { claudeSessionExists } = await import('../../bin/lib/runtime/index.js') as any;
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-home-'));
    fs.mkdirSync(path.join(home, '.claude', 'projects', '-proj'), { recursive: true });
    fs.writeFileSync(path.join(home, '.claude', 'projects', '-proj', 'abc-123.jsonl'), '{}');
    expect(claudeSessionExists('abc-123', home)).toBe(true);
    expect(claudeSessionExists('gone-456', home)).toBe(false);
    expect(claudeSessionExists('../../etc/passwd', home)).toBe(false);
    fs.rmSync(home, { recursive: true, force: true });
  });
});
