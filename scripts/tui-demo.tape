# ─────────────────────────────────────────────────────────────────────────
# Spaces TUI demo — renders public/tui-demo.gif with VHS (charmbracelet/vhs).
#
# WHAT THIS SHOWS
#   The terminal client (`spaces tui`): the agent-state sidebar (spaces +
#   agents with ◐/✓/○ glyphs and the "N blocked · N working · N done"
#   rollup), the command palette, and pane navigation — all keyboard-driven.
#
# PREREQUISITES (read before rendering)
#   1. VHS installed:  https://github.com/charmbracelet/vhs
#        brew install vhs   |   go install github.com/charmbracelet/vhs@latest
#      VHS runs best on Linux/macOS. On Windows, render under WSL.
#   2. `spaces` on PATH (npm i -g @jlongo78/agent-spaces) and a server already running:
#        spaces            # in another terminal, leave it up
#   3. THE IMPORTANT ONE — have a few agents in MIXED STATES running before
#      you render, or the sidebar is empty and the demo falls flat. Easiest:
#      run the seed script first, which stands up a demo workspace of agents
#      in mixed states (working ◐ / done ✓ / idle ○):
#        node scripts/seed-demo.js
#      (It spawns real agents and spends tokens — see its header.) Or start a
#      few panes by hand in the web UI / TUI. The sidebar reads real live state.
#
# RENDER
#   node scripts/seed-demo.js        # optional: populate the sidebar
#   vhs scripts/tui-demo.tape        # writes public/tui-demo.gif
#
# TUNING (you'll likely want one pass after the first render)
#   - The palette queries below ("new", "claude", "codex") are placeholders —
#     change them to match YOUR actual space/pane/agent names.
#   - Bump the Sleep values if a state hasn't visibly settled on screen.
#   - Keybindings are real: ctrl+k = command palette; ctrl+space then a
#     letter = chord (s = split, d = detach, m = toggle mouse).
# ─────────────────────────────────────────────────────────────────────────

Output public/tui-demo.gif

Require spaces

# ── Look & feel ──────────────────────────────────────────────────────────
Set Shell        bash
Set FontSize     16
Set Width        1200
Set Height       720
Set Padding      24
Set TypingSpeed  70ms
Set Theme        "Catppuccin Mocha"
Set CursorBlink  false

# ── Clean the prompt before we start recording anything meaningful ────────
Hide
Type "clear"
Enter
Show

# ── 1. Cold open: launch the terminal client ─────────────────────────────
#     The sidebar (spaces + agents + state rollup) paints on connect. This
#     is the hero frame — hold on it so the state glyphs read.
Type "spaces tui"
Enter
Sleep 4s

# ── 2. Command palette: everything is one fuzzy search away ───────────────
Ctrl+K
Sleep 1500ms
Type "new"
Sleep 2s
# Enter picks the highlighted "New pane in <space>" entry, which opens the
# agent picker (the palette re-opens with the agent list).
Enter
Sleep 1500ms
Type "claude"
Sleep 1s
Enter
Sleep 3s

# ── 3. Jump to a working agent — watch the ◐ spinner and live output ──────
Ctrl+K
Sleep 1s
# Replace "claude" with the name of a pane that's actively working.
Type "claude"
Sleep 1500ms
Enter
Sleep 3s

# ── 4. Jump to the one that needs you (blocked / done) ────────────────────
#     The sidebar rollup updates as focus moves; land on a pane that's
#     waiting so the "blocked" state is on screen.
Ctrl+K
Sleep 1s
Type "codex"
Sleep 1500ms
Enter
Sleep 3s

# ── 5. Split the view — two agents side by side ───────────────────────────
Ctrl+Space
Sleep 250ms
Type "s"
Sleep 3s

# ── 6. Detach — the sessions keep running on the server ───────────────────
Ctrl+Space
Sleep 250ms
Type "d"
Sleep 2s
