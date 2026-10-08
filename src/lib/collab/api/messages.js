'use strict';

const { getBroker } = require('../broker');
const queries = require('../queries');

exports.GET = async function GET(request, ctx) {
  const { id } = await ctx.params;
  const workspaceId = parseInt(id, 10);

  const sp = request.nextUrl.searchParams;
  const since = sp.get('since') || undefined;
  const type = sp.get('type') || undefined;
  const limit = sp.get('limit') ? parseInt(sp.get('limit'), 10) : undefined;

  const messages = getBroker().read(workspaceId, { since, type, limit });
  return Response.json(messages);
};

exports.POST = async function POST(request, ctx) {
  const { id } = await ctx.params;
  const workspaceId = parseInt(id, 10);
  const body = await request.json();

  const message = getBroker().post({
    workspaceId,
    paneId: body.paneId || null,
    senderName: body.senderName || 'User',
    type: body.type || 'message',
    content: body.content,
    targetPaneId: body.targetPaneId || null,
    metadata: body.metadata,
    isAgent: body.isAgent || false,
  });

  return Response.json(message);
};

exports.DELETE = async function DELETE(request, ctx) {
  const { id } = await ctx.params;
  queries.clearWorkspaceMessages(parseInt(id, 10));
  return Response.json({ success: true });
};
