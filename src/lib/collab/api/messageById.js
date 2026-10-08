'use strict';

const { getBroker } = require('../broker');

exports.PATCH = async function PATCH(request, ctx) {
  const { msgId } = await ctx.params;
  const messageId = parseInt(msgId, 10);
  const body = await request.json();

  if (body.status === 'approved') {
    const message = getBroker().approve(messageId);
    return Response.json(message);
  } else if (body.status === 'rejected') {
    const message = getBroker().reject(messageId);
    return Response.json(message);
  }

  return Response.json({ error: 'Invalid status' }, { status: 400 });
};
