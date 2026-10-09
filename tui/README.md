# Terminal client

`spaces` (or `spaces tui`) opens the full-screen terminal client: every space
and agent in a sidebar, the active space's panes as tabs, and the focused
pane taking the rest of the screen. It talks to a Spaces server over the
documented [`/proto` protocol](../docs/protocol/README.md); it does not embed
one.

## Starting it

```sh
spaces                    # in a terminal; falls back to the web app if no client binary
spaces tui                # the client explicitly, never the browser
spaces tui --desktop      # even if multiuser mode is saved in server.json
spaces tui --port 4000    # a server on another port
spaces --remote you@devbox    # Spaces on another machine, over SSH
```

If no server is running on the port, one is started in the background
(output in `~/.spaces/server.log`) and keeps running after the client exits.
`spaces stop` shuts it down.

The server answers only you. The launcher signs the client in with a session
token signed with a secret in `~/.spaces` that only your account can read;
other accounts on the same machine, and web pages in your browser, get
nothing. To open the web app signed in, run `spaces open`.

Multiuser mode refuses `/proto`, so the client cannot run against it. The
launcher says so and starts nothing, unless `--desktop` overrides the saved
setting.

## Another machine

`spaces --remote you@devbox` runs the client against Spaces on another
machine. It uses `ssh` for everything: `spaces start` over SSH starts the
server there if it is not running and hands back a session token, then a
tunnel carries the client to it. The remote server keeps listening on its
own loopback only, the remote's secret never leaves it, and authentication
is whatever your SSH setup already does, so keys, agents, `~/.ssh/config`
aliases and jump hosts all work and nothing new is exposed.

The destination is anything `ssh` accepts: `you@devbox`, an alias from
`~/.ssh/config`, or `ssh://you@devbox:2222`. If Spaces is missing on the
other machine, or older than yours, it offers to install or update it
first, saying how many running agents an update would stop; `--yes` skips
the questions. The remote server keeps running after you detach; `ssh you@devbox spaces stop`
shuts it down.

Under `--remote`, the Files tab shows the remote machine, downloads land on
your own machine, and uploads go to the remote.

## Keys

| Key | Does |
| --- | --- |
| `ctrl+k` | Open or close the menu. Type to filter, Enter to run. |
| `ctrl+space d` | Detach: quit the client, leaving every agent running. |
| `ctrl+space s` | Split the view to show a second pane, or unsplit it. |
| `ctrl+space f` | Open the active space's Files tab. |
| `ctrl+space b` | Hide or show the sidebar. |
| `ctrl+space o` | Sort the agents panel by space or by priority. |
| `ctrl+space m` | Release the mouse so the terminal can select and copy text; again to take it back. |

Every other key goes to the focused pane, `ctrl+c` included: interrupting an
agent is the most common thing anyone does here.

Everything in the table is also in the menu, along with focusing, closing,
restarting and creating panes, creating, renaming and colouring spaces, and
starting an agent, where it offers to resume one of the folder's past
sessions.

## The screen

- **Spaces**, top of the sidebar: one entry per space, with its agents'
  combined state. A space inside a git repository shows its branch on a
  second line, `↑`/`↓` for commits ahead of and behind its upstream, and `*`
  when the working tree has changes. `new` creates a space; `menu` opens the
  menu. Selecting a space whose panes a restart or reboot stopped starts
  them again.
- **Agents**, below: every agent and its state, `×` blocked, `◐` working,
  `✓` done, `○` idle. The header toggles `grouped`/`priority` sorting.
- **Tabs**: the active space's panes, and `[files]` once its Files tab is
  open. `+` adds a pane;
  `<` and `>` appear when the tabs do not all fit. Right-click a tab to close
  its pane or move it to another space.
- **Toasts**, top right: an agent in another pane that blocks or finishes
  says so for a few seconds. Click one to go to that pane.

With the mouse: click anything to focus it, drag the sidebar's right edge to
resize it, and drag the line between spaces and agents to split the sidebar
differently. Below about 80 columns the sidebar hides itself.

Changes made in the web app, such as a space renamed or a pane added, appear
here within about a second, and the reverse holds too.

## The Files tab

`ctrl+space f` opens the active space's Files tab: a file browser for the
machine the space runs on. The list is on the left; the selected file
previews on the right, with syntax highlighting.

| Key | Does |
| --- | --- |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn`, `g` `G` | Move the selection. |
| `Enter` / `→` / `l` | Open a folder, or view a file in full. |
| `Backspace` / `←` / `h` | Up one folder (the `..` row and the breadcrumbs work too). |
| `Tab` | Move between the list and the preview. |
| `/` | Filter the list; `Esc` clears. |
| `~` | Back to the starting folder. `r` reloads. |
| `d` | Download the selected file or folder into your Downloads folder. |
| `o` | Download an image or PDF and open it on your machine. |
| `u` | Upload a file or folder: type its local path. |
| `n` | New folder. |
| `m` / `F2` | Rename, or type a path to move. |
| `x` / `Delete` | Delete, after you confirm. |
| `e` | Edit a text file. |
| `Esc` | Close the preview, then the tab. |

**Drag and drop.** Drop files or folders from your file manager onto the
tab to upload them into the folder on screen. Your terminal delivers a drop
as the files' paths; the client recognises text that arrives faster than
anyone types as a paste, so the letters in a dropped path are never taken
as commands. If some names already exist, you are asked once whether to
replace them, upload only the others, or cancel.

**Editing.** `e` opens a small editor for quick fixes: `ctrl+s` saves,
`Esc` leaves (twice to discard changes). It keeps the file's line endings,
final newline and indentation style. If the file changed on the server
since you opened it, the save is refused; saving again overwrites.

**What it can reach.** Only your dev directories (`devDirectories` in
`~/.spaces/config.json`, or your home folder when none are set) and the
agents' transcript folders. Credential folders such as `~/.spaces`, `~/.ssh`,
`~/.aws` and `~/.gnupg` are refused even inside a dev directory, and
symlinks cannot lead outside. Opening locally (`o`) is limited to images and
PDFs, so nothing downloaded is ever run.

## Building from source

```sh
cd tui
cargo build --release -p spaces-tui
```

`spaces tui` finds a binary built this way in a checkout before looking for
an installed package. Close a running client first: on Windows the build
cannot replace an executable that is in use.

The binary itself takes `--url ws://host:port` and a session token in
`SPACES_TOKEN` (or `--token`); the launcher fills in both.
