'use strict';

const fs = require('fs');

/**
 * One line from the terminal, synchronously, or null at end of input.
 *
 * Synchronous on purpose: the launcher decides what to run by falling
 * through a chain of top-level checks, and a question answered on a later
 * tick would let that chain carry on to the next command meanwhile.
 *
 * Reads fd 0 directly, so nothing in the process may touch `process.stdin`
 * first: that sets up Node's own reader on the terminal, and this then
 * waits forever. Use `require('tty').isatty(0)` to ask whether it is one.
 */
function readLine({ read = (buf) => fs.readSync(0, buf, 0, 1, null) } = {}) {
  const byte = Buffer.alloc(1);
  const bytes = [];
  for (;;) {
    let n;
    try {
      n = read(byte);
    } catch (e) {
      // A non-blocking stdin has nothing yet: wait a moment and try again.
      if (e.code === 'EAGAIN') { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20); continue; }
      if (e.code === 'EOF') n = 0;
      else throw e;
    }
    if (n === 0) return bytes.length ? Buffer.from(bytes).toString('utf-8') : null;
    if (byte[0] === 0x0a) return Buffer.from(bytes).toString('utf-8').replace(/\r$/, '');
    bytes.push(byte[0]);
  }
}

/**
 * Asks `question` until the answer starts with one of `choices` (single
 * letters), and returns that letter. An empty answer returns `fallback`
 * when there is one and asks again when there is not; end of input returns
 * `fallback`, or null.
 */
function ask(question, choices, fallback, { write = (s) => process.stdout.write(s), read } = {}) {
  for (;;) {
    write(question);
    const line = readLine(read ? { read } : undefined);
    if (line === null) {
      write('\n');
      return fallback;
    }
    const answer = line.trim().toLowerCase();
    if (!answer && fallback) return fallback;
    const hit = choices.find((c) => answer === c || isWord(answer, c));
    if (hit) return hit;
  }
}

// "yes" counts as y and "quit" as q, but "yikes" is not an answer.
const WORDS = { y: ['yes'], n: ['no'], u: ['update'], c: ['connect'], q: ['quit'] };
function isWord(answer, letter) {
  return (WORDS[letter] || []).includes(answer);
}

module.exports = { ask, readLine };
