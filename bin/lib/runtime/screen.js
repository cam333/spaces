'use strict';

// @xterm/headless ships CJS; Terminal is on .Terminal (or default interop).
const headless = require('@xterm/headless');
const Terminal = headless.Terminal || headless.default?.Terminal;
const serializeMod = require('@xterm/addon-serialize');
const SerializeAddon = serializeMod.SerializeAddon || serializeMod.default?.SerializeAddon;

/**
 * Server-side screen model for one pane. Feeds detection,
 * replay-on-attach, and OSC title/progress capture.
 */
class PaneScreen {
  constructor({ cols = 120, rows = 30, onTitle, onProgress } = {}) {
    this._term = new Terminal({ cols, rows, scrollback: 5000, allowProposedApi: true });
    this._serialize = new SerializeAddon();
    this._term.loadAddon(this._serialize);
    this.title = '';
    this.progress = null; // raw OSC 9;4 payload after "9;", e.g. "4;3;"

    const titleHandler = (data) => {
      this.title = data;
      if (onTitle) { try { onTitle(data); } catch { /* callback errors stay local */ } }
      return false; // let xterm's own handling continue
    };
    this._term.parser.registerOscHandler(0, titleHandler);
    this._term.parser.registerOscHandler(2, titleHandler);
    this._term.parser.registerOscHandler(9, (data) => {
      if (data.startsWith('4;')) {
        this.progress = data;
        if (onProgress) { try { onProgress(data); } catch { /* ignore */ } }
      }
      return false;
    });
  }

  write(data, cb) { this._term.write(data, cb); }

  /** Live visible rows (bottom of buffer), scrollback excluded. */
  visibleGrid() {
    const buf = this._term.buffer.active;
    const out = [];
    for (let i = 0; i < this._term.rows; i++) {
      const line = buf.getLine(buf.baseY + i);
      out.push(line ? line.translateToString(true) : '');
    }
    return out;
  }

  detectionSnapshot() {
    return {
      rows: this.visibleGrid(),
      title: this.title,
      progress: this.progress,
      cols: this._term.cols,
      rows_count: this._term.rows,
    };
  }

  replayAnsi() { return this._serialize.serialize(); }

  resize(cols, rows) { this._term.resize(cols, rows); }

  dispose() { this._term.dispose(); }
}

module.exports = { PaneScreen };
