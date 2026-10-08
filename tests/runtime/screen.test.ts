import { describe, it, expect } from 'vitest';
import { PaneScreen } from '../../bin/lib/runtime/screen.js';

const write = (s: PaneScreen, data: string) =>
  new Promise<void>(resolve => (s as any).write(data, resolve));

describe('PaneScreen', () => {
  it('reflects written text in the visible grid', async () => {
    const screen = new PaneScreen({ cols: 40, rows: 5 });
    await write(screen, 'hello world\r\n$ ');
    const grid = screen.visibleGrid();
    expect(grid[0]).toContain('hello world');
    expect(grid[1]).toContain('$');
    screen.dispose();
  });

  it('visible grid tracks the bottom when output scrolls past rows', async () => {
    const screen = new PaneScreen({ cols: 40, rows: 3 });
    for (let i = 1; i <= 10; i++) await write(screen, `line-${i}\r\n`);
    const grid = screen.visibleGrid();
    // last write left cursor on a fresh row; visible rows end at line-10
    expect(grid.join('\n')).toContain('line-10');
    expect(grid.join('\n')).not.toContain('line-1\n');
    screen.dispose();
  });

  it('captures OSC 0/2 title and OSC 9;4 progress', async () => {
    const titles: string[] = [];
    const progress: string[] = [];
    const screen = new PaneScreen({
      cols: 40, rows: 5,
      onTitle: t => titles.push(t),
      onProgress: p => progress.push(p),
    });
    await write(screen, '\x1b]0;my title\x07');
    await write(screen, '\x1b]2;second title\x07');
    await write(screen, '\x1b]9;4;3;\x07'); // progress: busy
    expect(titles).toEqual(['my title', 'second title']);
    expect(screen.detectionSnapshot().title).toBe('second title');
    expect(progress).toEqual(['4;3;']);
    screen.dispose();
  });

  it('replayAnsi round-trips content into a fresh screen', async () => {
    const a = new PaneScreen({ cols: 40, rows: 5 });
    await write(a, 'persisted output\r\n\x1b[31mred\x1b[0m\r\n');
    const replay = a.replayAnsi();
    const b = new PaneScreen({ cols: 40, rows: 5 });
    await write(b, replay);
    expect(b.visibleGrid().join('\n')).toContain('persisted output');
    expect(b.visibleGrid().join('\n')).toContain('red');
    a.dispose(); b.dispose();
  });

  it('resize changes reported dimensions', async () => {
    const screen = new PaneScreen({ cols: 40, rows: 5 });
    screen.resize(100, 20);
    const snap = screen.detectionSnapshot();
    expect(snap.cols).toBe(100);
    expect(snap.rows_count).toBe(20);
    screen.dispose();
  });
});
