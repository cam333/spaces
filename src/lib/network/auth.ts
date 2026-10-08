import { validateApiKey } from './db';
import type { ApiKeyRecord } from '../../types/network';

export function validateNetworkAuth(request: Request): ApiKeyRecord | null {
  const authHeader = request.headers.get('authorization');
  if (!authHeader) return null;

  const match = authHeader.match(/^Bearer\s+(spk_[0-9a-f]{64})$/);
  if (!match) return null;

  return validateApiKey(match[1]);
}

export function requireNetworkAuth(request: Request): ApiKeyRecord {
  const key = validateNetworkAuth(request);
  if (!key) {
    throw new NetworkAuthError('Invalid or missing API key');
  }
  return key;
}

export class NetworkAuthError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'NetworkAuthError';
  }
}
