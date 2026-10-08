import path from 'path';
import os from 'os';
import { readFile } from 'fs/promises';

export interface ClaudeAiOAuthTokens {
  accessToken: string;
  refreshToken: string | null;
  expiresAt: number | null; // epoch ms
  scopes?: string[];
}

function getCredentialsPath(): string {
  const home = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
  return path.join(home, '.credentials.json');
}

/**
 * Reads ~/.claude/.credentials.json and returns the claudeAiOauth block.
 * Returns null on any read/parse failure or if the block is absent.
 */
export async function readClaudeAiOauthTokens(): Promise<ClaudeAiOAuthTokens | null> {
  try {
    const raw = await readFile(getCredentialsPath(), 'utf8');
    const parsed = JSON.parse(raw) as { claudeAiOauth?: ClaudeAiOAuthTokens };
    const tokens = parsed.claudeAiOauth;
    if (!tokens?.accessToken) return null;
    return tokens;
  } catch {
    return null;
  }
}

export function isTokenExpired(tokens: ClaudeAiOAuthTokens, nowMs = Date.now()): boolean {
  if (!tokens.expiresAt) return false; // no expiry set → treat as valid
  return nowMs >= tokens.expiresAt;
}
