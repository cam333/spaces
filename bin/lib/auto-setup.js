// Non-interactive first-time setup for Spaces multiuser mode.
// Called automatically when SPACES_MULTIUSER=1 but admin.db or session_secret are missing.
// For interactive customization, use: spaces --setup

const Database = require('better-sqlite3');
const crypto = require('crypto');
const path = require('path');
const fs = require('fs');
const os = require('os');

function hashPassword(password) {
  const salt = crypto.randomBytes(16).toString('hex');
  const hash = crypto.scryptSync(password, salt, 64).toString('hex');
  return `${salt}:${hash}`;
}

function generatePassword() {
  const chars = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789';
  let password = '';
  const bytes = crypto.randomBytes(16);
  for (let i = 0; i < 16; i++) {
    password += chars[bytes[i] % chars.length];
  }
  return password;
}

function autoSetup({ SPACES_DIR, SESSION_SECRET_PATH, INTERNAL_TOKEN_PATH, ADMIN_DB_PATH, CONFIG_PATH, port, basePath }) {
  // Ensure ~/.spaces directory
  if (!fs.existsSync(SPACES_DIR)) {
    fs.mkdirSync(SPACES_DIR, { recursive: true });
  }

  // Generate session secret (used to sign session cookies)
  let sessionSecret;
  if (fs.existsSync(SESSION_SECRET_PATH)) {
    sessionSecret = fs.readFileSync(SESSION_SECRET_PATH, 'utf-8').trim();
  }
  if (!sessionSecret) {
    sessionSecret = crypto.randomBytes(32).toString('hex');
    fs.writeFileSync(SESSION_SECRET_PATH, sessionSecret, { mode: 0o600 });
  }

  // Generate internal token (used by local hooks/MCP tools to authenticate
  // to the API without a session cookie). Separate from sessionSecret so
  // that if a user accidentally commits a per-project spaces-env.json the
  // session-signing key is not exposed — only this revocable token is.
  let internalToken;
  if (INTERNAL_TOKEN_PATH && fs.existsSync(INTERNAL_TOKEN_PATH)) {
    internalToken = fs.readFileSync(INTERNAL_TOKEN_PATH, 'utf-8').trim();
  }
  if (INTERNAL_TOKEN_PATH && !internalToken) {
    internalToken = crypto.randomBytes(32).toString('hex');
    fs.writeFileSync(INTERNAL_TOKEN_PATH, internalToken, { mode: 0o600 });
  }

  // Create admin DB and user
  let generatedPassword = null;
  if (!fs.existsSync(ADMIN_DB_PATH)) {
    const db = new Database(ADMIN_DB_PATH);
    db.pragma('journal_mode = WAL');
    db.pragma('synchronous = NORMAL');
    db.pragma('foreign_keys = ON');

    db.exec(`
      CREATE TABLE IF NOT EXISTS users (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL UNIQUE,
        password_hash TEXT NOT NULL,
        display_name TEXT NOT NULL DEFAULT '',
        shell_user TEXT NOT NULL,
        role TEXT NOT NULL DEFAULT 'user',
        totp_secret TEXT,
        totp_enabled INTEGER DEFAULT 0,
        created TEXT DEFAULT (datetime('now'))
      );
    `);

    generatedPassword = generatePassword();
    let shellUser = os.userInfo().username;
    // When running as SYSTEM (Windows service), resolve to the actual user
    // from the USERPROFILE env var set in the service wrapper script
    if (process.platform === 'win32' && shellUser.toUpperCase() === 'SYSTEM' && process.env.USERPROFILE) {
      const candidate = path.basename(process.env.USERPROFILE);
      // Validate: must be a plausible Windows username (no path traversal or special chars)
      if (/^[a-zA-Z0-9][a-zA-Z0-9 ._-]{0,63}$/.test(candidate)) {
        shellUser = candidate;
      }
    }
    const id = crypto.randomUUID();
    const passwordHash = hashPassword(generatedPassword);

    db.prepare(`
      INSERT INTO users (id, username, password_hash, display_name, shell_user, role)
      VALUES (?, ?, ?, ?, ?, 'admin')
    `).run(id, 'admin', passwordHash, 'Admin', shellUser);

    db.close();
  }

  // Save server.json if it doesn't exist
  if (!fs.existsSync(CONFIG_PATH)) {
    const config = { multiuser: true, port: port || 3457, basePath: basePath || '' };
    fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2));
  }

  // Print credentials box
  if (generatedPassword) {
    console.log('');
    console.log('  ┌──────────────────────────────────────────┐');
    console.log('  │  First-time setup complete                │');
    console.log('  │                                           │');
    console.log('  │  Username:  admin                         │');
    console.log(`  │  Password:  ${generatedPassword}                │`);
    console.log('  │                                           │');
    console.log('  │  Run `spaces --setup` to customize.       │');
    console.log('  └──────────────────────────────────────────┘');
    console.log('');
  }

  return { sessionSecret, internalToken, generatedPassword };
}

module.exports = { autoSetup, hashPassword, generatePassword };
