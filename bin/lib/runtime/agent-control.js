'use strict';

const PROMPT_STALL_MS = 5000;

/**
 * Encode a prompt for writing into a pane. Bracketed paste keeps a
 * multi-line prompt from being interpreted line-by-line by the agent's
 * readline. The trailing newline is stripped so the appended CR is the
 * only submit.
 */
function encodePrompt(text, { bracketedPaste = false } = {}) {
  const body = String(text ?? '').replace(/\r?\n$/, '');
  if (!bracketedPaste) return `${body}\r`;
  // A literal end-of-paste marker inside the body would close the pasted
  // region early, dumping the remainder as live keystrokes into the agent's
  // TTY instead of pasted text — strip it before wrapping.
  const safeBody = body.replace(/\x1b\[201~/g, '');
  return `\x1b[200~${safeBody}\x1b[201~\r`;
}

module.exports = { encodePrompt, PROMPT_STALL_MS };
