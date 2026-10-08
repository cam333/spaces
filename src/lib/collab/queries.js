'use strict';

const { getDeps } = require('./init');

function getDb() {
  return getDeps().getDb();
}

// ─── Workspace Messages ─────────────────────────────────────

exports.getWorkspaceMessages = function getWorkspaceMessages(workspaceId, opts) {
  opts = opts || {};
  const db = getDb();
  const conditions = ['workspace_id = ?'];
  const params = [workspaceId];

  if (opts.since) {
    conditions.push('created > ?');
    // Normalize ISO timestamps to SQLite format
    params.push(opts.since.replace('T', ' ').replace(/\.\d{3}Z$/, '').replace('Z', ''));
  }
  if (opts.type) {
    conditions.push('type = ?');
    params.push(opts.type);
  }

  const limit = opts.limit || 100;
  const offset = opts.offset || 0;

  return db.prepare(`
    SELECT id, workspace_id as workspaceId, pane_id as paneId,
           sender_name as senderName, type, content,
           target_pane_id as targetPaneId, status, created, metadata
    FROM workspace_messages
    WHERE ${conditions.join(' AND ')}
    ORDER BY created DESC
    LIMIT ? OFFSET ?
  `).all(...params, limit, offset);
};

exports.createWorkspaceMessage = function createWorkspaceMessage(msg) {
  const db = getDb();
  const result = db.prepare(`
    INSERT INTO workspace_messages (workspace_id, pane_id, sender_name, type, content, target_pane_id, status, metadata)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
  `).run(
    msg.workspaceId,
    msg.paneId || null,
    msg.senderName,
    msg.type || 'message',
    msg.content,
    msg.targetPaneId || null,
    msg.status || 'delivered',
    JSON.stringify(msg.metadata || {})
  );

  return db.prepare(`
    SELECT id, workspace_id as workspaceId, pane_id as paneId,
           sender_name as senderName, type, content,
           target_pane_id as targetPaneId, status, created, metadata
    FROM workspace_messages WHERE id = ?
  `).get(result.lastInsertRowid);
};

exports.updateMessageStatus = function updateMessageStatus(messageId, status) {
  const db = getDb();
  db.prepare('UPDATE workspace_messages SET status = ? WHERE id = ?').run(status, messageId);
};

exports.getMessageById = function getMessageById(messageId) {
  const db = getDb();
  return db.prepare(`
    SELECT id, workspace_id as workspaceId, pane_id as paneId,
           sender_name as senderName, type, content,
           target_pane_id as targetPaneId, status, created, metadata
    FROM workspace_messages WHERE id = ?
  `).get(messageId) || null;
};

// ─── Workspace Context (Shared Scratchpad) ──────────────────

exports.getWorkspaceContext = function getWorkspaceContext(workspaceId, key) {
  const db = getDb();
  if (key) {
    const row = db.prepare(`
      SELECT workspace_id as workspaceId, key, value, updated_by as updatedBy, updated
      FROM workspace_context WHERE workspace_id = ? AND key = ?
    `).get(workspaceId, key);
    return row ? [row] : [];
  }
  return db.prepare(`
    SELECT workspace_id as workspaceId, key, value, updated_by as updatedBy, updated
    FROM workspace_context WHERE workspace_id = ? ORDER BY key
  `).all(workspaceId);
};

exports.setWorkspaceContext = function setWorkspaceContext(workspaceId, key, value, updatedBy) {
  const db = getDb();
  db.prepare(`
    INSERT INTO workspace_context (workspace_id, key, value, updated_by, updated)
    VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
    ON CONFLICT(workspace_id, key) DO UPDATE SET value = ?, updated_by = ?, updated = CURRENT_TIMESTAMP
  `).run(workspaceId, key, value, updatedBy, value, updatedBy);
};

exports.deleteWorkspaceContext = function deleteWorkspaceContext(workspaceId, key) {
  const db = getDb();
  db.prepare('DELETE FROM workspace_context WHERE workspace_id = ? AND key = ?').run(workspaceId, key);
};

exports.clearWorkspaceMessages = function clearWorkspaceMessages(workspaceId) {
  const db = getDb();
  db.prepare('DELETE FROM workspace_messages WHERE workspace_id = ?').run(workspaceId);
};

exports.clearWorkspaceContext = function clearWorkspaceContext(workspaceId) {
  const db = getDb();
  db.prepare('DELETE FROM workspace_context WHERE workspace_id = ?').run(workspaceId);
};

exports.getCollaboratingPanes = function getCollaboratingPanes(workspaceId) {
  const db = getDb();
  return db.prepare(`
    SELECT id, title, color, cwd, claude_session_id as claudeSessionId,
           grid_col as gridCol, grid_row as gridRow, sort_order as sortOrder,
           shell, created, workspace_id as workspaceId, is_popout as isPopout,
           win_x as winX, win_y as winY, win_width as winWidth, win_height as winHeight,
           agent_type as agentType, custom_command as customCommand, node_id as nodeId,
           is_collaborating as isCollaborating
    FROM panes WHERE workspace_id = ? AND is_collaborating = 1 ORDER BY sort_order, created
  `).all(workspaceId);
};
