import { describe, it, expect } from 'vitest';
import { isPublicAssetPath } from '@/lib/auth/public-paths';

describe('isPublicAssetPath', () => {
  it('allows real static assets', () => {
    expect(isPublicAssetPath('/favicon.ico')).toBe(true);
    expect(isPublicAssetPath('/logo.png')).toBe(true);
    expect(isPublicAssetPath('/_next/static/chunk.js')).toBe(true);
    expect(isPublicAssetPath('/styles/app.css')).toBe(true);
  });

  it('NEVER bypasses /api/ paths even with an asset suffix', () => {
    expect(isPublicAssetPath('/api/proxy/models/x/v1/chat/x.js')).toBe(false);
    expect(isPublicAssetPath('/api/network/nodes/evil.png')).toBe(false);
    expect(isPublicAssetPath('/api/anything.css')).toBe(false);
  });

  it('does not bypass normal app routes', () => {
    expect(isPublicAssetPath('/')).toBe(false);
    expect(isPublicAssetPath('/terminal')).toBe(false);
  });
});
