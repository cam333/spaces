'use strict';

const EventEmitter = require('events');
const { getDeps } = require('./init');
const queries = require('./queries');

class MessageBroker extends EventEmitter {
  /**
   * Post a message to a workspace's bus.
   * Agent-initiated requests get status 'pending_approval' and wait for human action.
   * Everything else is delivered immediately.
   */
  post(msg) {
    const isRequest = msg.type === 'request' && msg.isAgent;
    const status = isRequest ? 'pending_approval' : 'delivered';

    const message = queries.createWorkspaceMessage({
      workspaceId: msg.workspaceId,
      paneId: msg.paneId,
      senderName: msg.senderName,
      type: msg.type || 'message',
      content: msg.content,
      targetPaneId: msg.targetPaneId,
      status,
      metadata: msg.metadata,
    });

    const { sseManager } = getDeps();

    if (isRequest) {
      sseManager.broadcast('workspace:request', message);
    } else {
      this.emit(`workspace:${msg.workspaceId}`, message);
      sseManager.broadcast('workspace:message', message);
    }

    return message;
  }

  /**
   * Approve a pending request. Changes status and broadcasts delivery.
   */
  approve(messageId) {
    queries.updateMessageStatus(messageId, 'approved');
    const message = queries.getMessageById(messageId);
    if (message) {
      const { sseManager } = getDeps();
      this.emit(`workspace:${message.workspaceId}`, message);
      sseManager.broadcast('workspace:message', message);
    }
    return message;
  }

  /**
   * Reject a pending request.
   */
  reject(messageId) {
    queries.updateMessageStatus(messageId, 'rejected');
    const message = queries.getMessageById(messageId);
    if (message) {
      const { sseManager } = getDeps();
      sseManager.broadcast('workspace:message', message);
    }
    return message;
  }

  /**
   * Read recent messages for a workspace.
   */
  read(workspaceId, opts) {
    return queries.getWorkspaceMessages(workspaceId, opts);
  }
}

let _broker = null;

exports.createBroker = function createBroker() {
  if (!_broker) _broker = new MessageBroker();
  return _broker;
};

exports.getBroker = function getBroker() {
  if (!_broker) _broker = new MessageBroker();
  return _broker;
};
