import { listNodes, getNodeApiKey } from './db';
import { getNodeIdentity } from './identity';
import { NetworkClient } from './client';

interface AggregationError {
  nodeId: string;
  nodeName: string;
  error: string;
}

export async function aggregateSessions(
  localResult: { sessions: any[]; total: number },
  params?: Record<string, string>
): Promise<{
  sessions: any[];
  total: number;
  errors: AggregationError[];
}> {
  const nodes = listNodes().filter(n => n.status !== 'offline');
  const errors: AggregationError[] = [];

  const allSessions: any[] = localResult.sessions.map(s => ({ ...s }));
  let totalCount = localResult.total;

  await Promise.allSettled(
    nodes.map(async (node) => {
      const apiKey = getNodeApiKey(node.id);
      if (!apiKey) {
        errors.push({ nodeId: node.id, nodeName: node.name, error: 'No API key' });
        return;
      }

      const client = new NetworkClient(node, apiKey);
      const result = await client.getSessions(params);

      if (result.ok) {
        for (const session of result.data.sessions) {
          allSessions.push({
            ...session,
            nodeId: node.id,
            nodeName: node.name,
          });
        }
        totalCount += result.data.total;
      } else {
        errors.push({ nodeId: node.id, nodeName: node.name, error: result.error });
      }
    })
  );

  // Sort merged results by modified date descending
  allSessions.sort((a, b) => {
    const da = new Date(b.modified).getTime();
    const db = new Date(a.modified).getTime();
    return da - db;
  });

  return { sessions: allSessions, total: totalCount, errors };
}

export async function aggregateSearch(
  localResults: any[],
  query: string,
  params?: Record<string, string>
): Promise<{
  results: any[];
  errors: AggregationError[];
}> {
  const nodes = listNodes().filter(n => n.status !== 'offline');
  const errors: AggregationError[] = [];

  const allResults: any[] = localResults.map(r => ({ ...r }));

  await Promise.allSettled(
    nodes.map(async (node) => {
      const apiKey = getNodeApiKey(node.id);
      if (!apiKey) {
        errors.push({ nodeId: node.id, nodeName: node.name, error: 'No API key' });
        return;
      }

      const client = new NetworkClient(node, apiKey);
      const result = await client.search(query, params);

      if (result.ok) {
        for (const r of result.data.results) {
          allResults.push({
            ...r,
            nodeId: node.id,
            nodeName: node.name,
          });
        }
      } else {
        errors.push({ nodeId: node.id, nodeName: node.name, error: result.error });
      }
    })
  );

  // Sort by rank (lower is better)
  allResults.sort((a, b) => a.rank - b.rank);

  return { results: allResults, errors };
}

export async function aggregateWorkspaces(): Promise<{
  nodes: { nodeId: string; nodeName: string; workspaces: any[] }[];
  errors: AggregationError[];
}> {
  const nodes = listNodes().filter(n => n.status !== 'offline');
  const errors: AggregationError[] = [];
  const nodeResults: { nodeId: string; nodeName: string; workspaces: any[] }[] = [];

  await Promise.allSettled(
    nodes.map(async (node) => {
      const apiKey = getNodeApiKey(node.id);
      if (!apiKey) {
        errors.push({ nodeId: node.id, nodeName: node.name, error: 'No API key' });
        return;
      }

      const client = new NetworkClient(node, apiKey);
      const result = await client.getWorkspaces();

      if (result.ok) {
        const workspaces = result.data.workspaces.map(ws => ({
          ...ws,
          nodeId: node.id,
          nodeName: node.name,
        }));
        nodeResults.push({ nodeId: node.id, nodeName: node.name, workspaces });
      } else {
        errors.push({ nodeId: node.id, nodeName: node.name, error: result.error });
      }
    })
  );

  return { nodes: nodeResults, errors };
}
