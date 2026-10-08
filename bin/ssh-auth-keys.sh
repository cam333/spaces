#!/bin/bash
# ──────────────────────────────────────────────────────────────
# Spaces SSH AuthorizedKeysCommand
# Called by sshd on every SSH login to dynamically return authorized keys.
#
# sshd_config usage:
#   AuthorizedKeysCommand /opt/spaces/bin/ssh-auth-keys.sh %u
#   AuthorizedKeysCommandUser nobody
#
# Arguments:
#   $1 - the SSH username being authenticated
#
# Outputs the Spaces service public key if the username is a valid
# Spaces shell_user in the admin database. This eliminates the need
# to manually manage ~/.ssh/authorized_keys for each user.
# ──────────────────────────────────────────────────────────────

USERNAME="$1"

if [ -z "$USERNAME" ]; then
  exit 0
fi

# Strict allowlist for Unix usernames. The match below is the only thing
# preventing SQL injection in the lookup — keep this conservative.
if ! echo "$USERNAME" | grep -qE '^[a-zA-Z0-9._-]+$'; then
  exit 0
fi

# SECURITY (review H3): resolve the .spaces directory from a single root-owned
# pin file written at install time (/opt/spaces/spaces-dir). The previous
# implementation globbed /home/*/.spaces and trusted the first match, so any
# local user could plant /home/<them>/.spaces/service_key.pub + admin.db and
# have sshd authorize THEIR key for arbitrary users (untrusted search path).
PIN_FILE="/opt/spaces/spaces-dir"

if [ ! -f "$PIN_FILE" ]; then
  # Fail closed: without a pinned, install-time path we do not guess.
  exit 0
fi

# The pin file must be owned by root, or we do not trust its contents.
PIN_OWNER=$(stat -c '%U' "$PIN_FILE" 2>/dev/null || stat -f '%Su' "$PIN_FILE" 2>/dev/null)
if [ "$PIN_OWNER" != "root" ]; then
  exit 0
fi

SPACES_DIR=$(head -n 1 "$PIN_FILE" | tr -d '\r\n')

# The pinned directory must exist.
if [ -z "$SPACES_DIR" ] || [ ! -d "$SPACES_DIR" ]; then
  exit 0
fi

SERVICE_KEY_PUB="$SPACES_DIR/service_key.pub"
USER_KEY_PUB="$SPACES_DIR/keys/$USERNAME.pub"
ADMIN_DB="$SPACES_DIR/admin.db"

# Restrict the authorized key to loopback connections (review M1): even if a
# private key leaks, it cannot authenticate from another host. We do NOT use the
# 'restrict' keyword because it implies no-pty, which would break interactive
# terminals.
KEY_OPTS='from="127.0.0.1,::1"'

# Emit the PER-USER key if present (review M1: one keypair per shell user, so a
# leaked key is not a universal credential), else fall back to the shared
# service key during the transition. Nothing is emitted if neither exists.
emit_authorized_key() {
  if [ -f "$USER_KEY_PUB" ]; then
    echo "$KEY_OPTS $(cat "$USER_KEY_PUB")"
  elif [ -f "$SERVICE_KEY_PUB" ]; then
    echo "$KEY_OPTS $(cat "$SERVICE_KEY_PUB")"
  fi
}

# Single-user / desktop mode: no admin DB. Authorize the key — there is exactly
# one OS user, so user-level isolation is not in scope.
if [ ! -f "$ADMIN_DB" ]; then
  emit_authorized_key
  exit 0
fi

# Multi-user mode: sqlite3 is required to verify the username against the
# admin DB. If it's missing, fail CLOSED — emit nothing — rather than
# authorize every login (the previous behaviour effectively bypassed
# multi-user isolation on minimal containers).
if ! command -v sqlite3 >/dev/null 2>&1; then
  exit 0
fi

# Belt-and-braces: escape any single quotes before interpolating. The regex
# above already excludes them, so this is no-op today but means a future
# regex relaxation can't immediately reopen SQL injection.
ESCAPED_USERNAME=${USERNAME//\'/\'\'}

MATCH=$(sqlite3 -readonly "$ADMIN_DB" \
  "SELECT COUNT(*) FROM users WHERE shell_user = '$ESCAPED_USERNAME'" 2>/dev/null)

if [ "$MATCH" -gt 0 ] 2>/dev/null; then
  emit_authorized_key
fi
