import { describe, it, expect, beforeEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import Database from 'better-sqlite3';
import {
  openNodeDb,
  listNodes,
  getNode,
  pairNode,
  forgetNode,
  listUnpaired,
  setFingerprint,
  encryptApiKey,
  decryptApiKey,
} from '../../bin/lib/network/nodes.js';

let dbPath: string;

beforeEach(() => {
  dbPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'spaces-nodes-')), 'network.db');
});

describe('node store', () => {
  it('records a paired node with its public key', () => {
    const db = openNodeDb({ path: dbPath });
    pairNode(db, {
      id: 'gpu-uuid',
      name: 'gpu-box',
      url: 'https://gpu-box:3477',
      publicKey: 'AAAA',
      apiKey: 'secret-key',
    });
    const node = getNode(db, 'gpu-uuid');
    expect(node.name).toBe('gpu-box');
    expect(node.public_key).toBe('AAAA');
    expect(node.discovered_via).toBe('manual');
    expect(listNodes(db)).toHaveLength(1);
  });

  it('never stores an api key in the clear', () => {
    const db = openNodeDb({ path: dbPath });
    pairNode(db, { id: 'n', name: 'n', url: 'https://n', publicKey: 'K', apiKey: 'hunter2' });
    const raw = getNode(db, 'n');
    expect(raw.api_key_encrypted).not.toContain('hunter2');
    expect(raw.api_key_hint).not.toBe('hunter2');
    expect(decryptApiKey(raw.api_key_encrypted)).toBe('hunter2');
  });

  it('uses the same encryption the web app uses', () => {
    // src/lib/network/db.ts writes the same column. If the two disagree, one
    // side silently reads garbage — the failure shape of the panes.color
    // regression, which emptied the pane list rather than erroring.
    const encrypted = encryptApiKey('shared-secret');
    expect(encrypted.split(':')).toHaveLength(3);
    expect(decryptApiKey(encrypted)).toBe('shared-secret');
  });

  it('returns null rather than throwing on a corrupt encrypted key', () => {
    expect(decryptApiKey('not-a-valid-blob')).toBeNull();
    expect(decryptApiKey('')).toBeNull();
  });

  it('opens a database written before public keys existed', () => {
    // A network.db created by the shipped web app has no public_key or
    // cert_fingerprint column. Selecting a missing column throws, and the
    // caller swallows it — so the visible symptom would be "no peers".
    const legacy = new Database(dbPath);
    legacy.exec(`
      CREATE TABLE nodes (
        id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL,
        api_key_encrypted TEXT NOT NULL DEFAULT '', api_key_hint TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'unknown', last_seen TEXT, last_error TEXT,
        session_count INTEGER DEFAULT 0, version TEXT,
        discovered_via TEXT DEFAULT 'manual',
        created TEXT DEFAULT (datetime('now')), updated TEXT DEFAULT (datetime('now'))
      );
      INSERT INTO nodes (id, name, url) VALUES ('old', 'old-box', 'https://old');
    `);
    legacy.close();

    const db = openNodeDb({ path: dbPath });
    const nodes = listNodes(db);
    expect(nodes).toHaveLength(1);
    expect(nodes[0].name).toBe('old-box');
    expect(nodes[0].public_key).toBeNull();

    // …and can be upgraded in place without losing the existing row.
    pairNode(db, { id: 'old', name: 'old-box', url: 'https://old', publicKey: 'NEW', apiKey: 'k' });
    expect(getNode(db, 'old').public_key).toBe('NEW');
  });

  it('separates discovered peers from paired ones', () => {
    // mDNS inserts rows with no key. Discovery is not trust, and the sidebar
    // has to be able to say so.
    const db = openNodeDb({ path: dbPath });
    db.prepare(
      `INSERT INTO nodes (id, name, url, api_key_encrypted, discovered_via)
       VALUES ('seen', 'studio-pc', 'http://studio-pc:3477', '', 'mdns')`
    ).run();
    pairNode(db, { id: 'known', name: 'gpu', url: 'https://gpu', publicKey: 'K', apiKey: 'k' });

    const unpaired = listUnpaired(db);
    expect(unpaired.map((n: any) => n.id)).toEqual(['seen']);
  });

  it('counts a machine with an api key but no keypair as unpaired', () => {
    // Rows written by the older federation path look like this. An API key is
    // not something a machine can prove it holds, so it is not a trust anchor
    // and must not be mistaken for one — the two must not be conflated.
    const db = openNodeDb({ path: dbPath });
    db.prepare(
      `INSERT INTO nodes (id, name, url, api_key_encrypted, discovered_via)
       VALUES ('legacy', 'old-peer', 'http://old', 'iv:tag:data', 'manual')`
    ).run();

    expect(listUnpaired(db).map((n: any) => n.id)).toEqual(['legacy']);
  });

  it('pins a fingerprint and forgets a node completely', () => {
    const db = openNodeDb({ path: dbPath });
    pairNode(db, { id: 'n', name: 'n', url: 'https://n', publicKey: 'K', apiKey: 'k' });
    setFingerprint(db, 'n', 'AB:CD:EF');
    expect(getNode(db, 'n').cert_fingerprint).toBe('AB:CD:EF');

    expect(forgetNode(db, 'n')).toBe(true);
    expect(getNode(db, 'n')).toBeNull();
    expect(forgetNode(db, 'n')).toBe(false);
  });

  it('re-pairing replaces the key rather than keeping the old one', () => {
    // Rotating a peer's key must not leave the superseded key able to connect.
    const db = openNodeDb({ path: dbPath });
    pairNode(db, { id: 'n', name: 'n', url: 'https://n', publicKey: 'OLD', apiKey: 'k' });
    pairNode(db, { id: 'n', name: 'n', url: 'https://n', publicKey: 'ROTATED', apiKey: 'k' });
    expect(getNode(db, 'n').public_key).toBe('ROTATED');
    expect(listNodes(db)).toHaveLength(1);
  });
});
