'use strict';

const queries = require('../queries');

exports.GET = async function GET(request, ctx) {
  const { id } = await ctx.params;
  const entries = queries.getWorkspaceContext(parseInt(id, 10));
  return Response.json(entries);
};

exports.DELETE = async function DELETE(request, ctx) {
  const { id } = await ctx.params;
  queries.clearWorkspaceContext(parseInt(id, 10));
  return Response.json({ success: true });
};
