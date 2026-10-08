// NOTE: Zod schemas in this codebase do not use .strict() — the server
// silently strips unknown request keys rather than rejecting them. The
// exported JSON Schema (scripts/export-protocol-schema.mjs) matches that
// leniency: it passes `removeAdditionalStrategy: 'strict'` to
// zod-to-json-schema so every generated object schema allows additional
// properties instead of emitting the library's default `additionalProperties:
// false`. This keeps third-party JSON-Schema validators (like Rust
// serde_json) from hard-rejecting payloads the server actually accepts,
// including future additive v1 protocol fields.

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { WebSocketServer, WebSocket } from 'ws';
import fs from 'node:fs';
import path from 'node:path';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import { attachControlChannel } from '../../bin/lib/runtime/control-channel.js';
import { schemas } from '../../bin/lib/runtime/protocol.js';
import fsx from 'node:fs';
import pathx from 'node:path';
import Databasex from 'better-sqlite3';
import osx from 'node:os';
import { createRuntime as createRuntimeX, DISPATCH_METHODS } from '../../bin/lib/runtime/index.js';

const fx = (name: string) =>
  JSON.parse(fs.readFileSync(path.join(__dirname, 'fixtures', name), 'utf-8'));

describe('protocol contract fixtures', () => {
  // Scoped to this describe (rather than file-level) so the unrelated
  // session-control contract tests below don't each pay for spinning up a
  // real WebSocketServer they never connect to.
  let wss: WebSocketServer, port: number;

  beforeEach(async () => {
    const runtime = createRuntime({ nodeId: 'contract', spacesDbPath: '' });
    runtime.state.open('p1', { agentType: 'claude', cwd: '.', workspaceId: 'w1' });
    wss = new WebSocketServer({ host: '127.0.0.1', port: 0 });
    wss.on('connection', ws => attachControlChannel(ws, runtime, { authenticated: true }));
    await new Promise(r => wss.once('listening', r));
    port = (wss.address() as any).port;
  });
  afterEach(() => new Promise(r => wss.close(r)));

  async function roundtrip(messages: unknown[]): Promise<any[]> {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/proto`);
    await new Promise(r => ws.once('open', r));
    const replies: any[] = [];
    ws.on('message', d => replies.push(JSON.parse(d.toString())));
    for (const m of messages) {
      ws.send(JSON.stringify(m));
      await new Promise(r => setTimeout(r, 50));
    }
    ws.close();
    return replies;
  }

  it('hello fixture', async () => {
    const f = fx('hello.json');
    const [reply] = await roundtrip([f.send]);
    expect(reply).toMatchObject(f.expect);
    expect(schemas.HelloOk.safeParse(reply).success).toBe(true);
  });

  it('snapshot fixture', async () => {
    const f = fx('snapshot.json');
    const [, reply] = await roundtrip([fx('hello.json').send, f.send]);
    expect(Object.keys(reply.result).sort()).toEqual([...f.expectResultKeys].sort());
    expect(schemas.Snapshot.safeParse(reply.result).success).toBe(true);
  });

  it('events resume fixture', async () => {
    const f = fx('events-resume.json');
    const [, reply] = await roundtrip([fx('hello.json').send, f.send]);
    for (const k of f.expectResultKeys) expect(reply.result).toHaveProperty(k);
    for (const e of reply.result.events) {
      expect(schemas.EventFrame.safeParse({ event: e }).success).toBe(true);
    }
  });

  it('pane up-frame fixtures match the schema exactly', () => {
    const f = fx('pane-frames.json');
    for (const v of f.validUpFrames) expect(schemas.PaneUpFrame.safeParse(v).success).toBe(true);
    for (const v of f.invalidUpFrames) expect(schemas.PaneUpFrame.safeParse(v).success).toBe(false);
  });

  it('pane down-frame fixtures match the schema exactly', () => {
    const f = fx('pane-down-frames.json');
    for (const v of f.validDownFrames) expect(schemas.PaneDownFrame.safeParse(v).success).toBe(true);
    for (const v of f.invalidDownFrames) expect(schemas.PaneDownFrame.safeParse(v).success).toBe(false);
  });
});

describe('session-control contract', () => {
  const fxdir = pathx.join(__dirname, 'fixtures');
  const catalogue = JSON.parse(fsx.readFileSync(pathx.join(fxdir, 'method-catalogue.json'), 'utf-8'));
  const frames = JSON.parse(fsx.readFileSync(pathx.join(fxdir, 'session-control-frames.json'), 'utf-8'));

  // Windows cannot remove a directory containing a file with an open handle.
  // createSessionStore() keeps its better-sqlite3 connection open for the
  // runtime's lifetime by design, so every runtime created by runtimeWithDb()
  // below is tracked here and closed before its tmp dir is removed — mirrors
  // the pattern in tests/proto/session-control.test.ts.
  const runtimesX: any[] = [];
  const dirsX: string[] = [];

  afterEach(() => {
    for (const runtime of runtimesX.splice(0)) {
      try { runtime.sessionStore.close(); } catch { /* already closed */ }
    }
    for (const dir of dirsX.splice(0)) {
      fsx.rmSync(dir, { recursive: true, force: true });
    }
  });

  function runtimeWithDb() {
    process.env.SPACES_NO_DETECTION_UPDATE = '1';
    const dir = fsx.mkdtempSync(pathx.join(osx.tmpdir(), 'spaces-cc-'));
    const dbPath = pathx.join(dir, 'spaces.db');
    const db = new Databasex(dbPath);
    db.exec(`
      CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, description TEXT, color TEXT);
      CREATE TABLE panes (id TEXT PRIMARY KEY, title TEXT, color TEXT, cwd TEXT, agent_type TEXT DEFAULT 'shell', sort_order INTEGER DEFAULT 0, workspace_id INTEGER REFERENCES workspaces(id) ON DELETE CASCADE);
    `);
    db.close();
    const runtime = createRuntimeX({ nodeId: 'contract', spacesDbPath: dbPath });
    runtimesX.push(runtime);
    dirsX.push(dir);
    return { runtime, dir };
  }

  it('node.methods matches the committed catalogue exactly', () => {
    const { runtime } = runtimeWithDb();
    const res = (runtime.dispatch('node.methods', {}) as any).result;
    expect(res.protocolVersion).toBe(catalogue.protocolVersion);
    expect(res.sync).toEqual(catalogue.sync);
    expect(res.async).toEqual(catalogue.async);
  });

  it('DISPATCH_METHODS is exhaustive against the real dispatch switch (both directions)', () => {
    // The two tests above only ever compare DISPATCH_METHODS to the fixture
    // and the fixture to node.methods' response — none of that notices a
    // `case` added to the switch that nobody also added to DISPATCH_METHODS
    // (or the fixture): that method would just silently never appear in
    // node.methods. So read this runtime module's own source as text and
    // extract every `case '<name>':` label straight out of dispatch()'s
    // switch, then diff that set against DISPATCH_METHODS in BOTH
    // directions. This assumes the file has exactly one switch statement
    // (true today). If a second switch is ever added anywhere in this file,
    // this regex would pick up its labels too and this test would very
    // likely start failing with entries neither side expects — a false
    // alarm, which is the correct failure mode here: better a spurious
    // break than a silently incomplete node.methods catalogue.
    const srcPath = pathx.join(__dirname, '..', '..', 'bin', 'lib', 'runtime', 'index.js');
    const src = fsx.readFileSync(srcPath, 'utf-8');
    const inSwitch = new Set<string>();
    for (const m of src.matchAll(/case '([^']+)':/g)) inSwitch.add(m[1]);
    const catalogued = new Set(DISPATCH_METHODS);

    const inSwitchNotCatalogued = [...inSwitch].filter(m => !catalogued.has(m)).sort();
    const cataloguedNotInSwitch = [...catalogued].filter(m => !inSwitch.has(m)).sort();

    expect(inSwitchNotCatalogued, `in switch but not catalogued: ${inSwitchNotCatalogued.join(', ') || '(none)'}`).toEqual([]);
    expect(cataloguedNotInSwitch, `catalogued but not in switch: ${cataloguedNotInSwitch.join(', ') || '(none)'}`).toEqual([]);
  });

  it('every catalogued sync method is actually dispatchable', () => {
    const { runtime } = runtimeWithDb();
    for (const method of catalogue.sync) {
      const out = runtime.dispatch(method, {}) as any;
      // Must not be "unknown method" — a bad_request about params is fine.
      if (out.error) expect(out.error.message).not.toMatch(/unknown method/i);
    }
  });

  it('every new event type is in the EventFrame enum and its sample validates', () => {
    for (const type of frames.eventTypes) {
      const sample = frames.sampleEvents.find((e: any) => e.type === type);
      expect(sample, `no sample event for ${type}`).toBeDefined();
      expect(schemas.EventFrame.safeParse({ event: sample }).success, `${type} failed EventFrame`).toBe(true);
    }
  });

  it('the async methods refuse a synchronous dispatch', () => {
    const { runtime } = runtimeWithDb();
    for (const method of catalogue.async) {
      const out = runtime.dispatch(method, {}) as any;
      expect(out.error, `${method} should error synchronously`).toBeDefined();
      expect(out.error.message).toMatch(/asynchronous/i);
    }
  });
});
