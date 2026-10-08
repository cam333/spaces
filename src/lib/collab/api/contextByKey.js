'use strict';

const queries = require('../queries');
const { getDeps } = require('../init');

exports.GET = async function GET(request, ctx) {
  const { id, key } = await ctx.params;
  const entries = queries.getWorkspaceContext(parseInt(id, 10), decodeURIComponent(key));
  return Response.json(entries[0] || null);
};

exports.PUT = async function PUT(request, ctx) {
  const { id, key } = await ctx.params;
  const body = await request.json();
  const workspaceId = parseInt(id, 10);
  const decodedKey = decodeURIComponent(key);

  queries.setWorkspaceContext(workspaceId, decodedKey, body.value, body.updatedBy || 'User');

  const { sseManager } = getDeps();
  sseManager.broadcast('workspace:context_updated', { workspaceId, key: decodedKey });

  const entries = queries.getWorkspaceContext(workspaceId, decodedKey);
  return Response.json(entries[0]);
};

exports.DELETE = async function DELETE(request, ctx) {
  const { id, key } = await ctx.params;
  queries.deleteWorkspaceContext(parseInt(id, 10), decodeURIComponent(key));
  return Response.json({ success: true });
};
