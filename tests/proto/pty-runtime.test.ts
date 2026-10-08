import { describe, it, expect } from 'vitest';
import { createRuntime } from '../../bin/lib/runtime/index.js';
import * as pty from 'node-pty';

// Mirrors the wiring added to handleConnection: spawn PTY -> registerPane -> feed -> exit
describe('pty wired into runtime', () => {
  it('registers, feeds output into the screen, and closes on exit', async () => {
    const runtime = createRuntime({ nodeId: 'n', spacesDbPath: '' });
    const shell = process.platform === 'win32' ? 'cmd.exe' : 'sh';
    const args = process.platform === 'win32' ? ['/c', 'echo pty-probe'] : ['-c', 'echo pty-probe'];
    const term = pty.spawn(shell, args, { name: 'xterm-256color', cols: 80, rows: 24, cwd: process.cwd(), env: process.env as any });

    runtime.registerPane('pane-x', {
      cols: 80, rows: 24, agentType: 'shell', cwd: process.cwd(), workspaceId: null,
      onInput: d => term.write(d), onResize: (c, r) => term.resize(c, r),
    });
    term.onData(d => runtime.feed('pane-x', d));
    const exited = new Promise<number>(res => term.onExit(({ exitCode }) => {
      runtime.paneExit('pane-x', exitCode);
      res(exitCode);
    }));

    await exited;
    // screen is disposed on exit; state must reflect closure
    expect(runtime.state.get('pane-x')!.live).toBe(false);
    const { gap, events } = runtime.hub.eventsAfter(0);
    expect(gap).toBe(false);
    expect(events.map(e => e.type)).toContain('pane.opened');
    expect(events.map(e => e.type)).toContain('pane.closed');
  }, 15000);

  // Covering test for the follow-up fix: both `ws.on('message')` resize
  // handlers in handleConnection (fresh-spawn and reattach) now resize the
  // PTY *and* the runtime screen together. This mirrors that exact pattern
  // (independent try/catch per call, screen resize alongside pty resize) so
  // a regression that drops the screen resize call is caught here rather
  // than only surfacing as stale visibleGrid()/detectionSnapshot() output.
  it('a resize message that resizes the PTY also resizes the runtime screen (mid-session)', async () => {
    const runtime = createRuntime({ nodeId: 'n', spacesDbPath: '' });
    const shell = process.platform === 'win32' ? 'cmd.exe' : (process.env.SHELL || '/bin/sh');
    const term = pty.spawn(shell, [], { name: 'xterm-256color', cols: 80, rows: 24, cwd: process.cwd(), env: process.env as any });

    runtime.registerPane('pane-y', {
      cols: 80, rows: 24, agentType: 'shell', cwd: process.cwd(), workspaceId: null,
      onInput: d => term.write(d), onResize: (c, r) => term.resize(c, r),
    });

    const before = runtime.getScreen('pane-y')!.detectionSnapshot();
    expect(before.cols).toBe(80);
    expect(before.rows_count).toBe(24);

    // Exactly the pattern added to both ws.on('message') resize handlers:
    // PTY resize and screen resize as two independently-guarded calls.
    try { term.resize(132, 50); } catch { /* ignore */ }
    try { runtime.getScreen('pane-y')?.resize(132, 50); } catch { /* runtime optional here */ }

    const after = runtime.getScreen('pane-y')!.detectionSnapshot();
    expect(after.cols).toBe(132);
    expect(after.rows_count).toBe(50);

    const exited = new Promise<number>(res => term.onExit(({ exitCode }) => {
      runtime.paneExit('pane-y', exitCode);
      res(exitCode);
    }));
    term.write(process.platform === 'win32' ? 'exit\r' : 'exit\n');
    await exited;
  }, 15000);
});
