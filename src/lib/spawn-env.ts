// Build a minimal, safe environment for child processes that execute user-controlled
// code (PTY shells, `claude` CLI, etc). The server's own env contains secrets —
// SPACES_SESSION_SECRET, GH_PAT, NPM_TOKEN, ANTHROPIC_API_KEY — and any of those
// leaking into a user PTY would let the user forge sessions or impersonate the
// server against upstream services.

const ALLOWED_ENV_VARS = new Set([
  // Identity / shell basics
  'HOME', 'USER', 'LOGNAME', 'SHELL', 'PATH', 'PWD', 'OLDPWD', 'HOSTNAME',
  // Locale / time
  'LANG', 'LC_ALL', 'LC_CTYPE', 'LC_MESSAGES', 'LC_COLLATE', 'LC_TIME',
  'LC_NUMERIC', 'LC_MONETARY', 'TZ',
  // XDG base dirs
  'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR',
  'XDG_STATE_HOME',
  // Terminal / display
  'TERM', 'TERM_PROGRAM', 'TERM_PROGRAM_VERSION', 'COLORTERM', 'COLUMNS', 'LINES',
  // Node / package manager tooling child tools may need
  'NODE_ENV', 'NVM_DIR', 'NVM_BIN',
]);

export function buildSpawnEnv(extras: Record<string, string | undefined> = {}): NodeJS.ProcessEnv {
  const env: Record<string, string> = {};
  for (const key of ALLOWED_ENV_VARS) {
    const v = process.env[key];
    if (v !== undefined) env[key] = v;
  }
  for (const [k, v] of Object.entries(extras)) {
    if (v !== undefined) env[k] = v;
  }
  return env as unknown as NodeJS.ProcessEnv;
}
