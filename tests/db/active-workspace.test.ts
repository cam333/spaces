import { describe, it, expect, vi } from 'vitest';
import Database from 'better-sqlite3';

vi.mock('@/lib/config', () => ({ getUserPaths: vi.fn(), ensureUserSpacesDir: vi.fn() }));
vi.mock('@/lib/auth', () => ({ getCurrentUser: vi.fn() }));

const { ensureActiveWorkspace } = await import('@/lib/db/schema');

function freshDb() {
  const db = new Database(':memory:');
  db.exec(`
    CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
      description TEXT DEFAULT '', color TEXT DEFAULT '#6366f1', is_active INTEGER DEFAULT 0);
    CREATE TABLE panes (id TEXT PRIMARY KEY, workspace_id INTEGER);
  `);
  return db;
}
const workspaces = (db: Database.Database) =>
  db.prepare('SELECT name, is_active AS active FROM workspaces ORDER BY id').all();

describe('ensureActiveWorkspace', () => {
  it('creates an active Default workspace on a fresh install', () => {
    const db = freshDb();
    ensureActiveWorkspace(db);
    expect(workspaces(db)).toEqual([{ name: 'Default', active: 1 }]);
  });

  it('does not bring back a deleted Default, or hide the user\'s spaces behind it', () => {
    const db = freshDb();
    db.exec(`INSERT INTO workspaces (name, is_active) VALUES ('dev', 0), ('webshop', 0), ('empty', 0);
             INSERT INTO panes VALUES ('a', 1), ('b', 2), ('c', 2);`);
    ensureActiveWorkspace(db);
    expect(workspaces(db)).toEqual([
      { name: 'dev', active: 0 }, { name: 'webshop', active: 1 }, { name: 'empty', active: 0 },
    ]);
  });

  it('keeps the active workspace the user chose', () => {
    const db = freshDb();
    db.exec(`INSERT INTO workspaces (name, is_active) VALUES ('dev', 0), ('ops', 1);`);
    ensureActiveWorkspace(db);
    expect(workspaces(db)).toEqual([{ name: 'dev', active: 0 }, { name: 'ops', active: 1 }]);
  });

  it('leaves exactly one active workspace and gives orphan panes a home', () => {
    const db = freshDb();
    db.exec(`INSERT INTO workspaces (name, is_active) VALUES ('a', 1), ('b', 1);
             INSERT INTO panes VALUES ('orphan', NULL);`);
    ensureActiveWorkspace(db);
    expect(workspaces(db)).toEqual([{ name: 'a', active: 1 }, { name: 'b', active: 0 }]);
    expect(db.prepare("SELECT workspace_id FROM panes WHERE id = 'orphan'").get()).toEqual({ workspace_id: 1 });
  });
});
