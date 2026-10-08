import type { NodeRecord, HandshakeResponse } from '../../types/network';

export type NetworkResult<T> = { ok: true; data: T } | { ok: false; error: string; nodeId: string };

export class NetworkClient {
  private baseUrl: string;
  private apiKey: string;
  private nodeId: string;

  constructor(node: NodeRecord, apiKey: string) {
    this.baseUrl = node.url.replace(/\/+$/, '');
    this.apiKey = apiKey;
    this.nodeId = node.id;
  }

  private async request<T>(path: string, opts?: RequestInit): Promise<NetworkResult<T>> {
    const fetchOpts: RequestInit = {
      ...opts,
      headers: {
        'Authorization': `Bearer ${this.apiKey}`,
        'Content-Type': 'application/json',
        ...opts?.headers,
      },
      redirect: 'follow',
      signal: AbortSignal.timeout(10000),
    };

    // TLS verification stays ON by default — it is the only thing that
    // authenticates a federation peer's identity. Previously this code flipped
    // the process-global Node TLS kill-switch env var to '0', disabling
    // verification for every outbound TLS connection in the process and racing
    // with concurrent requests (review C3). If an operator must support
    // self-signed federation certs, they opt in explicitly and we scope the
    // relaxation to THIS request via a per-request undici dispatcher.
    if (this.baseUrl.startsWith('https') && process.env.SPACES_SKIP_TLS_VERIFY === '1') {
      try {
        const { Agent } = require('undici');
        // @ts-expect-error - dispatcher is an undici-specific fetch option
        fetchOpts.dispatcher = new Agent({ connect: { rejectUnauthorized: false } });
      } catch {
        console.warn('[NetworkClient] undici unavailable for per-request TLS opt-out; keeping full verification');
      }
    }

    try {
      const res = await fetch(`${this.baseUrl}${path}`, fetchOpts);

      if (!res.ok) {
        const text = await res.text();
        return { ok: false, error: `HTTP ${res.status}: ${text}`, nodeId: this.nodeId };
      }

      const data = await res.json() as T;
      return { ok: true, data };
    } catch (e: any) {
      return { ok: false, error: e.message || 'Request failed', nodeId: this.nodeId };
    }
  }

  async handshake(): Promise<NetworkResult<HandshakeResponse>> {
    return this.request('/api/network/handshake');
  }

  async health(): Promise<NetworkResult<{ status: string; nodeId: string; version: string; uptime: number }>> {
    return this.request('/api/network/health');
  }

  async getSessions(params?: Record<string, string>): Promise<NetworkResult<{ sessions: any[]; total: number }>> {
    const sp = new URLSearchParams(params || {});
    return this.request(`/api/network/sessions?${sp}`);
  }

  async getSession(id: string): Promise<NetworkResult<any>> {
    return this.request(`/api/network/sessions/${id}`);
  }

  async getMessages(sessionId: string, offset = 0, limit = 50): Promise<NetworkResult<{ messages: any[]; total: number; hasMore: boolean }>> {
    return this.request(`/api/network/sessions/${sessionId}/messages?offset=${offset}&limit=${limit}`);
  }

  async search(query: string, params?: Record<string, string>): Promise<NetworkResult<{ results: any[]; query: string }>> {
    const sp = new URLSearchParams({ q: query, ...params });
    return this.request(`/api/network/search?${sp}`);
  }

  async getProjects(): Promise<NetworkResult<any[]>> {
    return this.request('/api/network/projects');
  }

  async getWorkspaces(): Promise<NetworkResult<{ workspaces: any[]; total: number }>> {
    return this.request('/api/network/workspaces/');
  }

  async getWorkspacePanes(workspaceId: number): Promise<NetworkResult<{ panes: any[] }>> {
    return this.request(`/api/network/workspaces/${workspaceId}/`);
  }

  async getTerminalToken(): Promise<NetworkResult<{ token: string; wsUrl: string }>> {
    return this.request('/api/network/terminal/token', { method: 'POST' });
  }
}
