import fs from 'fs';
import os from 'os';
import path from 'path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';

import { __testing } from '@/lib/db/schema';
import {
  getWorkspaceTodos,
  addWorkspaceTodo,
  updateWorkspaceTodo,
  deleteWorkspaceTodo,
} from '@/lib/db/queries';

const TEST_USER = 'test-user'; // matches tests/setup.ts auth mock
let db: Database.Database;
let tempDir: string;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-todos-test-'));
  const dbPath = path.join(tempDir, 'test.db');
  db = new Database(dbPath);
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');

  const ddl = [
    `CREATE TABLE workspaces (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      color TEXT DEFAULT '#6366f1',
      created TEXT DEFAULT (datetime('now'))
    )`,
    `CREATE TABLE workspace_todos (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      workspace_id INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
      text TEXT NOT NULL,
      completed INTEGER NOT NULL DEFAULT 0,
      sort_order INTEGER NOT NULL DEFAULT 0,
      created TEXT NOT NULL DEFAULT (datetime('now')),
      completed_at TEXT
    )`,
    `CREATE INDEX idx_workspace_todos_workspace ON workspace_todos(workspace_id)`,
  ];
  for (const stmt of ddl) {
    db.prepare(stmt).run();
  }

  __testing.dbs.set(TEST_USER, db);

  db.prepare("INSERT INTO workspaces (id, name) VALUES (1, 'Test WS')").run();
});

afterEach(() => {
  try { db.close(); } catch {}
  __testing.dbs.delete(TEST_USER);
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe('workspace todos query helpers', () => {
  it('adds a todo with incrementing sort_order among uncompleted rows', () => {
    const a = addWorkspaceTodo(1, 'first');
    const b = addWorkspaceTodo(1, 'second');
    const c = addWorkspaceTodo(1, 'third');

    expect(a.sortOrder).toBe(0);
    expect(b.sortOrder).toBe(1);
    expect(c.sortOrder).toBe(2);
    expect(a.completed).toBe(false);
    expect(a.text).toBe('first');
  });

  it('trims text and rejects empty', () => {
    const t = addWorkspaceTodo(1, '  hello  ');
    expect(t.text).toBe('hello');

    expect(() => addWorkspaceTodo(1, '   ')).toThrow();
    expect(() => addWorkspaceTodo(1, '')).toThrow();
  });

  it('toggles completed and sets/clears completed_at', () => {
    const t = addWorkspaceTodo(1, 'toggle me');
    expect(t.completedAt).toBeNull();

    const done = updateWorkspaceTodo(1, t.id, { completed: true });
    expect(done?.completed).toBe(true);
    expect(done?.completedAt).not.toBeNull();

    const undone = updateWorkspaceTodo(1, t.id, { completed: false });
    expect(undone?.completed).toBe(false);
    expect(undone?.completedAt).toBeNull();
  });

  it('updates text, trims, rejects empty', () => {
    const t = addWorkspaceTodo(1, 'original');
    const upd = updateWorkspaceTodo(1, t.id, { text: '  edited  ' });
    expect(upd?.text).toBe('edited');

    expect(() => updateWorkspaceTodo(1, t.id, { text: '   ' })).toThrow();
  });

  it('lists todos: uncompleted first by sort_order, completed last by completed_at desc', () => {
    const a = addWorkspaceTodo(1, 'a');
    const b = addWorkspaceTodo(1, 'b');
    const c = addWorkspaceTodo(1, 'c');

    updateWorkspaceTodo(1, b.id, { completed: true });
    updateWorkspaceTodo(1, a.id, { completed: true });

    const list = getWorkspaceTodos(1);
    expect(list.map(t => t.text)).toEqual(['c', 'a', 'b']);
  });

  it('deletes a todo', () => {
    const t = addWorkspaceTodo(1, 'doomed');
    deleteWorkspaceTodo(1, t.id);
    expect(getWorkspaceTodos(1)).toHaveLength(0);
  });

  it('cascades when the workspace is deleted', () => {
    addWorkspaceTodo(1, 'x');
    addWorkspaceTodo(1, 'y');
    db.prepare('DELETE FROM workspaces WHERE id = 1').run();
    const rows = db.prepare('SELECT COUNT(*) AS n FROM workspace_todos').get() as { n: number };
    expect(rows.n).toBe(0);
  });

  it('scopes listing by workspace', () => {
    db.prepare("INSERT INTO workspaces (id, name) VALUES (2, 'Other')").run();
    addWorkspaceTodo(1, 'ws-1-a');
    addWorkspaceTodo(2, 'ws-2-a');
    addWorkspaceTodo(1, 'ws-1-b');

    const ws1 = getWorkspaceTodos(1);
    const ws2 = getWorkspaceTodos(2);
    expect(ws1.map(t => t.text).sort()).toEqual(['ws-1-a', 'ws-1-b']);
    expect(ws2.map(t => t.text)).toEqual(['ws-2-a']);
  });

  it('refuses to update or delete a todo from a different workspace', () => {
    db.prepare("INSERT INTO workspaces (id, name) VALUES (2, 'Other')").run();
    const t = addWorkspaceTodo(1, 'belongs to ws 1');

    // Update with the wrong workspace id → returns null, leaves row untouched.
    const update = updateWorkspaceTodo(2, t.id, { text: 'hijacked' });
    expect(update).toBeNull();
    expect(getWorkspaceTodos(1)[0].text).toBe('belongs to ws 1');

    // Delete with the wrong workspace id → row survives.
    deleteWorkspaceTodo(2, t.id);
    expect(getWorkspaceTodos(1)).toHaveLength(1);

    // Correct workspace → works.
    deleteWorkspaceTodo(1, t.id);
    expect(getWorkspaceTodos(1)).toHaveLength(0);
  });
});
