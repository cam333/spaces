//! Tips: one short line about using Spaces, picked at random and shown in
//! the status bar for a while each time the client starts.
//!
//! Turned off, for good, from the menu (`ctrl+k`, "tips") or with
//! `spaces tips off`; both write `"tips": false` to this machine's
//! `~/.spaces/tui.json`. The setting belongs to the machine the client runs
//! on, not the server it talks to, so a `--remote` session follows the
//! laptop's choice. `SPACES_NO_TIPS=1` turns them off for one run.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a tip stays in the status bar.
pub const TIP_TTL: Duration = Duration::from_secs(30);

/// Every tip, short enough for the status bar beside the tallies. Each is
/// true of the released client and CLI: a tip that names a key that does
/// nothing is worse than none.
pub const TIPS: &[&str] = &[
    // Agents and the screen.
    "ctrl+space d detaches: agents keep running without you",
    "ctrl+space s splits the view to watch two agents at once",
    "ctrl+space o sorts agents by priority, blocked first",
    "ctrl+space b hides the sidebar; ctrl+space m frees the mouse to select",
    "ctrl+k, then type part of a name to jump to any agent or space",
    "click a toast to jump to the agent that blocked or finished",
    "right-click a tab to close its pane or move it to another space",
    "drag the sidebar's edge to make it wider or narrower",
    "ctrl+k, then type theme: preview 18 colour themes and keep one",
    "the sidebar shows each space's git branch: ↑ ahead, ↓ behind, * dirty",
    "a new agent offers to resume one of the folder's past sessions",
    "Claude Code panes resume their conversation after a restart",
    "paste several lines into an agent: they arrive as one paste",
    // The Files tab: every key here is in files.rs.
    "ctrl+space f opens a file browser for the space's machine",
    "Files works on the remote machine too, under spaces --remote",
    "in Files, the selected file previews live, syntax highlighted",
    "in Files, tab moves into the preview to scroll it; tab again to go back",
    "in Files, o opens an image or PDF on this machine, even from a server",
    "in Files, d downloads a file, or a whole folder, to your Downloads",
    "in Files, u uploads; or drag files from your file manager onto it",
    "in Files, e edits a text file and ctrl+s saves it",
    "Files refuses a save if the file changed since you opened it",
    "in Files, / filters the list; ~ goes back to where Files starts",
    "in Files, n makes a folder, m or F2 renames or moves, x deletes",
    "in Files, ctrl+space s puts the files beside the terminal",
    "in Files, r refreshes the list; h and l, j and k move like vim",
    // Commands.
    "spaces --remote you@host runs your agents on another machine",
    "spaces web shows these same agents in your browser, signed in",
    "spaces web --remote you@host: the browser view of another machine",
    "the browser view searches every past Claude, Codex and Gemini session",
    "the browser view's dashboard shows token use and cost by model",
    "on a phone, the browser view at /m has quick keys and voice input",
    "spaces service install starts Spaces when you log in",
    "spaces status shows the version and how many agents are live",
    "agents in one space can message each other with spaces-msg",
    "spaces signout signs out every browser and terminal client",
    "tired of tips? ctrl+k, type tips, enter",
];

/// This machine's client settings file.
fn prefs_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".spaces").join("tui.json"))
}

fn read_prefs(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Whether tips are on in the settings at `path`: on unless they say off.
pub fn enabled_in(path: &std::path::Path) -> bool {
    read_prefs(path).get("tips").and_then(|v| v.as_bool()) != Some(false)
}

/// Whether to show a tip this run.
pub fn enabled() -> bool {
    if std::env::var_os("SPACES_NO_TIPS").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    prefs_path().is_none_or(|p| enabled_in(&p))
}

/// One setting from this machine's `tui.json`, if it has one. Shared with
/// [`crate::ui::theme`], whose choice lives in the same file.
pub fn read_pref(key: &str) -> Option<serde_json::Value> {
    read_prefs(&prefs_path()?).remove(key)
}

/// Saves one setting to this machine's `tui.json`. Best effort, like
/// [`save_enabled`].
pub fn save_pref(key: &str, value: serde_json::Value) {
    if let Some(p) = prefs_path() {
        let _ = save_pref_in(&p, key, value);
    }
}

/// Writes the setting to `path`, keeping anything else the file holds.
pub fn save_enabled_in(path: &std::path::Path, on: bool) -> std::io::Result<()> {
    save_pref_in(path, "tips", serde_json::Value::Bool(on))
}

/// Writes `key` to the settings at `path`, keeping anything else they hold.
pub fn save_pref_in(
    path: &std::path::Path,
    key: &str,
    value: serde_json::Value,
) -> std::io::Result<()> {
    let mut prefs = read_prefs(path);
    prefs.insert(key.into(), value);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(prefs))
        .unwrap_or_else(|_| "{}".into());
    std::fs::write(path, text + "\n")
}

/// Saves the setting for this machine. Best effort: a read-only home costs
/// the setting, not the session.
pub fn save_enabled(on: bool) {
    if let Some(p) = prefs_path() {
        let _ = save_enabled_in(&p, on);
    }
}

/// The tip for a given seed.
pub fn pick(seed: u64) -> &'static str {
    TIPS[(seed % TIPS.len() as u64) as usize]
}

/// A tip chosen by the clock.
pub fn random() -> &'static str {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // The low bits of a coarse clock repeat; mix them.
    pick(nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 17)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tip_fits_beside_the_tallies() {
        for tip in TIPS {
            assert!(
                tip.chars().count() <= 72,
                "too long for the status bar: {tip}"
            );
        }
    }

    #[test]
    fn every_seed_picks_a_tip() {
        for seed in 0..100 {
            assert!(TIPS.contains(&pick(seed)));
        }
    }

    #[test]
    fn tips_are_on_until_turned_off_and_the_rest_of_the_file_survives() {
        let dir = std::env::temp_dir().join(format!("spaces-tips-{}", std::process::id()));
        let path = dir.join("tui.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(enabled_in(&path), "no file: on");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"other": 1}"#).unwrap();
        save_enabled_in(&path, false).unwrap();
        assert!(!enabled_in(&path));
        let kept: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(kept["other"], 1);
        save_enabled_in(&path, true).unwrap();
        assert!(enabled_in(&path));
        save_pref_in(&path, "theme", serde_json::Value::String("nord".into())).unwrap();
        assert_eq!(read_prefs(&path)["theme"], "nord");
        assert!(enabled_in(&path), "saving the theme keeps the tips setting");
        std::fs::write(&path, "not json").unwrap();
        assert!(
            enabled_in(&path),
            "an unreadable file does not silence tips"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
