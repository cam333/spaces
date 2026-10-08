import fs from 'fs';
import os from 'os';
import path from 'path';
import { afterEach, describe, expect, it } from 'vitest';
import { scanCodexSessions } from './parser';

const tempDirs: string[] = [];

afterEach(() => {
  while (tempDirs.length) {
    const dir = tempDirs.pop();
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  }
});

describe('scanCodexSessions', () => {
  it('parses session metadata from payload-based rollout headers', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-parser-'));
    tempDirs.push(root);

    const sessionsDir = path.join(root, '2026', '04', '03');
    fs.mkdirSync(sessionsDir, { recursive: true });

    const filePath = path.join(
      sessionsDir,
      'rollout-2026-04-03T15-21-47-019d54cb-5d1e-7af2-aa51-56be8515390e.jsonl'
    );

    fs.writeFileSync(
      filePath,
      [
        JSON.stringify({
          timestamp: '2026-04-03T19:22:15.454Z',
          type: 'session_meta',
          payload: {
            id: '019d54cb-5d1e-7af2-aa51-56be8515390e',
            timestamp: '2026-04-03T19:21:47.044Z',
            cwd: 'C:\\projects\\spaces',
          },
        }),
        JSON.stringify({
          timestamp: '2026-04-03T19:22:16.000Z',
          payload: {
            type: 'user_message',
            text: 'testing codex resume metadata',
          },
        }),
      ].join('\n'),
      'utf8'
    );

    const sessions = scanCodexSessions(root);
    expect(sessions).toHaveLength(1);
    expect(sessions[0]).toMatchObject({
      id: '019d54cb-5d1e-7af2-aa51-56be8515390e',
      cwd: 'C:\\projects\\spaces',
      timestamp: '2026-04-03T19:21:47.044Z',
      firstPrompt: 'testing codex resume metadata',
    });
  });

  it('parses oversized session_meta lines emitted by newer Codex versions', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-parser-'));
    tempDirs.push(root);

    const sessionsDir = path.join(root, '2026', '04', '03');
    fs.mkdirSync(sessionsDir, { recursive: true });

    const filePath = path.join(
      sessionsDir,
      'rollout-2026-04-03T20-18-57-019d54ff-b56c-7972-959c-a2f6d20be10b.jsonl'
    );

    const oversizedMeta = JSON.stringify({
      timestamp: '2026-04-03T20:18:57.521Z',
      type: 'session_meta',
      payload: {
        id: '019d54ff-b56c-7972-959c-a2f6d20be10b',
        timestamp: '2026-04-03T20:18:57.000Z',
        cwd: 'C:\\projects\\spaces',
        base_instructions: {
          text: 'x'.repeat(20_000),
        },
      },
    });

    fs.writeFileSync(
      filePath,
      [
        oversizedMeta,
        JSON.stringify({
          timestamp: '2026-04-03T20:18:58.000Z',
          payload: {
            type: 'user_message',
            text: 'pane should reopen Codex history',
          },
        }),
      ].join('\n'),
      'utf8'
    );

    const sessions = scanCodexSessions(root);
    expect(sessions).toHaveLength(1);
    expect(sessions[0]).toMatchObject({
      id: '019d54ff-b56c-7972-959c-a2f6d20be10b',
      cwd: 'C:\\projects\\spaces',
      timestamp: '2026-04-03T20:18:57.000Z',
      firstPrompt: 'pane should reopen Codex history',
    });
  });
});
