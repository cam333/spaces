import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import { createRuntime } from '../../bin/lib/runtime/index.js';

const isWindows = process.platform === 'win32';
const api = isWindows ? 'C:\\work\\api' : '/work/api';
const web = isWindows ? 'C:\\work\\web' : '/work/web';

let dbPath: string;

/** A runtime over a database seeded with `rows` in a `sessions` table. */
function runtimeWithSessions(rows: Record<string, unknown>[], withTable = true) {
  const db = new Database(dbPath);
  db.exec(`
    CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, color TEXT);
    CREATE TABLE panes (id TEXT PRIMARY KEY, workspace_id INTEGER, agent_type TEXT, cwd TEXT);
  `);
  if (withTable) {
    db.exec(`
      CREATE TABLE sessions (
        id INTEGER PRIMARY KEY, session_id TEXT, project_id INTEGER,
        first_prompt TEXT, summary TEXT, message_count INTEGER,
        created TEXT, modified TEXT, git_branch TEXT, project_path TEXT,
        full_path TEXT, starred INTEGER, custom_name TEXT, agent_type TEXT
      );
    `);
    const insert = db.prepare(
      `INSERT INTO sessions
         (session_id, first_prompt, summary, message_count, modified, git_branch,
          project_path, custom_name, agent_type)
       VALUES (@session_id, @first_prompt, @summary, @message_count, @modified,
               @git_branch, @project_path, @custom_name, @agent_type)`
    );
    for (const r of rows) {
      insert.run({
        session_id: null, first_prompt: null, summary: null, message_count: null,
        modified: null, git_branch: null, project_path: null, custom_name: null,
        agent_type: 'claude', ...r,
      });
    }
  }
  db.close();
  return createRuntime({ nodeId: 'test', spacesDbPath: dbPath });
}

beforeEach(() => {
  dbPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-sess-')), 'spaces.db');
});

describe('session.list', () => {
  it('returns a directory’s sessions, newest first', () => {
    const runtime = runtimeWithSessions([
      { session_id: 's1', project_path: api, first_prompt: 'fix the parser',
        message_count: 12, modified: '2026-08-17T10:00:00Z', git_branch: 'main' },
      { session_id: 's2', project_path: api, custom_name: 'the good one',
        message_count: 3, modified: '2026-08-17T12:00:00Z' },
      { session_id: 's3', project_path: web, first_prompt: 'other directory' },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;

    expect(out.sessions.map((s: any) => s.sessionId)).toEqual(['s2', 's1']);
    expect(out.sessions[0].label).toBe('the good one');
    expect(out.sessions[1].label).toBe('fix the parser');
    expect(out.sessions[1].messageCount).toBe(12);
    expect(out.sessions[1].gitBranch).toBe('main');
  });

  it('resolves a label through every fallback', () => {
    // A client must never have to reimplement this precedence, so it is
    // pinned here rather than left to whichever field happens to be set.
    const runtime = runtimeWithSessions([
      { session_id: 'a', project_path: api, custom_name: 'named', summary: 's', first_prompt: 'f' },
      { session_id: 'b', project_path: api, summary: 'summarised', first_prompt: 'f' },
      { session_id: 'c', project_path: api, first_prompt: 'prompted' },
      { session_id: 'd', project_path: api },
    ]);
    const byId: Record<string, string> = {};
    for (const s of (runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result as any).sessions) {
      byId[s.sessionId] = s.label;
    }
    // `d` has nothing at all, so it says so rather than showing a bare id.
    expect(byId).toEqual({ a: 'named', b: 'summarised', c: 'prompted', d: 'unnamed · d' });
  });

  it('matches a directory written differently', () => {
    // The stored path and the one a client asks with need not agree
    // character for character.
    const runtime = runtimeWithSessions([
      { session_id: 's1', project_path: api + (isWindows ? '\\' : '/') },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions).toHaveLength(1);
  });

  it('honours a limit and caps it', () => {
    const many = Array.from({ length: 30 }, (_, i) => ({
      session_id: `s${i}`,
      project_path: api,
      modified: `2026-08-${String(i + 1).padStart(2, '0')}T00:00:00Z`,
    }));
    const runtime = runtimeWithSessions(many);
    const five: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude', limit: 5 }).result;
    expect(five.sessions).toHaveLength(5);
    expect(five.sessions[0].sessionId).toBe('s29', );
    const capped: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude', limit: 9999 }).result;
    expect(capped.sessions.length).toBeLessThanOrEqual(100);
    const def: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(def.sessions).toHaveLength(20);
  });

  it('returns nothing for an agent that cannot resume', () => {
    // Answered by the server so the agent table stays the only place that
    // knows which agents have a resume flag.
    const runtime = runtimeWithSessions([
      { session_id: 's1', project_path: api, agent_type: 'shell' },
    ]);
    for (const agentType of ['shell', 'aider']) {
      const out: any = runtime.dispatch('session.list', { cwd: api, agentType }).result;
      expect(out.sessions, agentType).toEqual([]);
    }
  });

  it('returns nothing for a directory with no sessions', () => {
    const runtime = runtimeWithSessions([{ session_id: 's1', project_path: web }]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions).toEqual([]);
  });

  it('survives a database with no sessions table', () => {
    // Older databases predate it. Selecting a missing table throws, and the
    // failure shape that matters is the one that empties a list silently.
    const runtime = runtimeWithSessions([], false);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions).toEqual([]);
  });

  it('rejects a call with no directory or no agent', () => {
    const runtime = runtimeWithSessions([]);
    expect(runtime.dispatch('session.list', { agentType: 'claude' }).error).toBeDefined();
    expect(runtime.dispatch('session.list', { cwd: api }).error).toBeDefined();
  });
});

describe('session labels', () => {
  it('strips the tags a harness wrapped round a prompt', () => {
    const runtime = runtimeWithSessions([
      { session_id: 's1', project_path: api, first_prompt: '<user-note>fix the parser</user-note>' },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions[0].label).toBe('fix the parser');
  });

  it('collapses whitespace so one row stays one line', () => {
    const runtime = runtimeWithSessions([
      { session_id: 's1', project_path: api, first_prompt: 'fix\n\n  the   parser\t' },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions[0].label).toBe('fix the parser');
  });

  it('trims a long prompt rather than letting it run', () => {
    const runtime = runtimeWithSessions([
      { session_id: 's1', project_path: api, first_prompt: 'x'.repeat(300) },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions[0].label.length).toBeLessThanOrEqual(72);
    expect(out.sessions[0].label.endsWith('…')).toBe(true);
  });

  it('refuses harness preamble as a description', () => {
    // Real rows in this database hold nothing but this. `first_prompt` is
    // truncated when stored, so the user's actual message was cut off before
    // it landed — stripping the prefix recovers nothing, and two such
    // sessions were indistinguishable from one another in a picker.
    const runtime = runtimeWithSessions([
      {
        session_id: '09c80812-aaaa-bbbb-cccc-dddddddddddd',
        project_path: api,
        first_prompt:
          '<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond',
      },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions[0].label).toBe('unnamed · 09c80812');
  });

  it('says a row is unnamed rather than showing punctuation', () => {
    const runtime = runtimeWithSessions([
      { session_id: 'abcdef12-0000-0000-0000-000000000000', project_path: api, first_prompt: '--- ...' },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions[0].label).toBe('unnamed · abcdef12');
  });

  it('prefers a real name over a caveat even when both are present', () => {
    const runtime = runtimeWithSessions([
      {
        session_id: 's1',
        project_path: api,
        custom_name: 'the good one',
        first_prompt: 'Caveat: The messages below were generated by the user',
      },
    ]);
    const out: any = runtime.dispatch('session.list', { cwd: api, agentType: 'claude' }).result;
    expect(out.sessions[0].label).toBe('the good one');
  });
});
