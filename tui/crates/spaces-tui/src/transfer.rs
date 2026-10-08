//! Downloads and uploads for the Files tab.
//!
//! A transfer is many `fs.read` / `fs.write` calls of at most 1 MiB, so it
//! runs on a connection of its own in a spawned task and reports back over a
//! channel: the event loop keeps drawing and taking keys meanwhile, and the
//! main connection is never held for longer than one UI request.
//!
//! A folder goes file by file, its tree recreated on the other side.
//! Symlinked folders are skipped in both directions, so a link that loops
//! back on itself cannot make a transfer run forever, and a transfer stops
//! at [`MAX_FILES`] rather than quietly copying a whole disk.
//!
//! Names from the server become file names on this machine, so they are
//! cleaned for the local filesystem first. Opening a file here is limited to
//! images and PDFs: "open" on an executable or a shortcut would run it.

use std::path::{Path, PathBuf};

use base64::Engine;
use spaces_proto::client::ControlClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// Bytes per `fs.read` / `fs.write` call; the server's own ceiling.
const CHUNK: u64 = 1024 * 1024;
/// Per-call budget: one chunk over a slow link, not a whole transfer.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Most files one folder transfer will move.
pub const MAX_FILES: usize = 20_000;

/// Where a transfer reports to, per space.
#[derive(Debug)]
pub enum TransferMsg {
    Progress {
        space: String,
        label: String,
        done: u64,
        total: u64,
    },
    Finished {
        space: String,
        note: String,
        /// A remote folder whose listing changed.
        relist: Option<String>,
        /// A local file to open now.
        open: Option<PathBuf>,
    },
    Failed {
        space: String,
        note: String,
    },
}

#[derive(Debug, Clone)]
pub enum JobKind {
    Download {
        remote: String,
        size: u64,
        /// Open it in its default app once it has arrived.
        open: bool,
    },
    /// A whole remote folder, into the local Downloads folder.
    DownloadFolder { remote: String },
    /// A local file or folder into `remote_dir`.
    Upload {
        local: PathBuf,
        remote_dir: String,
        overwrite: bool,
    },
    /// Several, one after another on one connection.
    UploadMany {
        locals: Vec<PathBuf>,
        remote_dir: String,
        overwrite: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Job {
    pub space: String,
    pub node: Option<String>,
    pub kind: JobKind,
}

/// The connection details a transfer dials with.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub url: String,
    pub token: Option<String>,
    pub tx: mpsc::Sender<TransferMsg>,
}

/// Runs `job` in the background.
pub fn spawn(ctx: &Ctx, job: Job) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let space = job.space.clone();
        let outcome = match ControlClient::connect(&ctx.url, ctx.token.as_deref()).await {
            Ok(client) => {
                let mut run = Run {
                    client,
                    node: job.node.clone(),
                    space: space.clone(),
                    tx: ctx.tx.clone(),
                    label: String::new(),
                    done: 0,
                    total: 0,
                };
                match &job.kind {
                    JobKind::Download { remote, size, open } => {
                        run.download_file(remote, *size, *open).await
                    }
                    JobKind::DownloadFolder { remote } => run.download_folder(remote).await,
                    JobKind::Upload {
                        local,
                        remote_dir,
                        overwrite,
                    } => run.upload(local, remote_dir, *overwrite).await,
                    JobKind::UploadMany {
                        locals,
                        remote_dir,
                        overwrite,
                    } => run.upload_many(locals, remote_dir, *overwrite).await,
                }
            }
            Err(e) => Err(format!("could not connect: {e}")),
        };
        let msg = match outcome {
            Ok(done) => done,
            Err(note) => TransferMsg::Failed { space, note },
        };
        let _ = ctx.tx.send(msg).await;
    });
}

/// One transfer in progress: its connection, and the running byte count it
/// reports against.
struct Run {
    client: ControlClient,
    node: Option<String>,
    space: String,
    tx: mpsc::Sender<TransferMsg>,
    label: String,
    done: u64,
    total: u64,
}

/// One file of a folder transfer.
struct Item {
    remote: String,
    local: PathBuf,
    size: u64,
}

impl Run {
    async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        tokio::time::timeout(
            CALL_TIMEOUT,
            crate::request_on(&mut self.client, self.node.as_deref(), method, params),
        )
        .await
        .unwrap_or_else(|_| Err("timed out".into()))
    }

    async fn progress(&mut self, bytes: u64) {
        self.done += bytes;
        let _ = self
            .tx
            .send(TransferMsg::Progress {
                space: self.space.clone(),
                label: self.label.clone(),
                done: self.done,
                total: self.total,
            })
            .await;
    }

    /// Copies one remote file to `dest`, through a `.part` file that is
    /// renamed into place only when complete.
    async fn fetch(&mut self, remote: &str, dest: &Path) -> Result<(), String> {
        // A name of its own, created fresh: never another program's file
        // (browsers use `<name>.part` for their own downloads).
        let part = dest.with_file_name(format!(
            "{}.{:x}.spaces-part",
            dest.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download"),
            random_u64()
        ));
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .await
                .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
            let mut offset = 0u64;
            loop {
                let reply = self
                    .call(
                        "fs.read",
                        serde_json::json!({ "path": remote, "offset": offset, "length": CHUNK }),
                    )
                    .await?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(reply.get("data").and_then(|d| d.as_str()).unwrap_or(""))
                    .map_err(|_| "the server sent unreadable data".to_string())?;
                file.write_all(&bytes)
                    .await
                    .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
                offset += bytes.len() as u64;
                self.progress(bytes.len() as u64).await;
                if reply.get("eof").and_then(|e| e.as_bool()).unwrap_or(true) || bytes.is_empty() {
                    break;
                }
            }
            file.flush().await.map_err(|e| e.to_string())?;
            drop(file);
            tokio::fs::rename(&part, dest)
                .await
                .map_err(|e| format!("cannot finish {}: {e}", dest.display()))
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&part).await;
        }
        result
    }

    /// Copies one local file to `dest`, through a temporary name on the
    /// server that is moved into place only when complete.
    async fn send(&mut self, local: &Path, dest: &str, overwrite: bool) -> Result<(), String> {
        let part = format!("{dest}.spaces-upload");
        let result = async {
            let mut file = tokio::fs::File::open(local)
                .await
                .map_err(|e| format!("cannot read {}: {e}", local.display()))?;
            let mut buf = vec![0u8; CHUNK as usize];
            let mut first = true;
            loop {
                let n = read_full(&mut file, &mut buf)
                    .await
                    .map_err(|e| format!("cannot read {}: {e}", local.display()))?;
                if n == 0 && !first {
                    break;
                }
                let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                self.call(
                    "fs.write",
                    serde_json::json!({
                        "path": part,
                        "data": data,
                        "mode": if first { "truncate" } else { "append" },
                    }),
                )
                .await?;
                first = false;
                self.progress(n as u64).await;
                if n < buf.len() {
                    break;
                }
            }
            self.call(
                "fs.move",
                serde_json::json!({ "from": part, "to": dest, "overwrite": overwrite }),
            )
            .await
            .map(|_| ())
        }
        .await;
        if result.is_err() {
            let _ = self
                .call("fs.delete", serde_json::json!({ "path": part }))
                .await;
        }
        result
    }

    async fn download_file(
        &mut self,
        remote: &str,
        size: u64,
        open: bool,
    ) -> Result<TransferMsg, String> {
        let name = safe_local_name(remote);
        let dir = if open { open_dir()? } else { downloads_dir() };
        let dest = unique_path(&dir, &name);
        self.label = format!("downloading {name}");
        self.total = size;
        self.fetch(remote, &dest).await?;
        Ok(TransferMsg::Finished {
            space: self.space.clone(),
            note: if open {
                format!("opened {name}")
            } else {
                format!("saved to {}", dest.display())
            },
            relist: None,
            open: open.then_some(dest),
        })
    }

    async fn download_folder(&mut self, remote: &str) -> Result<TransferMsg, String> {
        let name = safe_local_name(remote);
        let root = unique_path(&downloads_dir(), &name);
        self.label = format!("listing {name}");
        // Walk first, so progress has a total and a too-big folder is
        // refused before anything is written.
        let mut items = Vec::new();
        let mut skipped = 0usize;
        let mut folders = vec![(remote.to_string(), root.clone())];
        let mut index = 0;
        while index < folders.len() {
            let (dir, local) = folders[index].clone();
            index += 1;
            let listing = self
                .call("fs.list", serde_json::json!({ "path": dir }))
                .await?;
            if listing
                .get("truncated")
                .and_then(|t| t.as_bool())
                .unwrap_or(false)
            {
                return Err(format!("{dir} has too many entries to download"));
            }
            for entry in listing
                .get("entries")
                .and_then(|e| e.as_array())
                .into_iter()
                .flatten()
            {
                let (Some(entry_name), Some(path), Some(kind)) = (
                    entry.get("name").and_then(|v| v.as_str()),
                    entry.get("path").and_then(|v| v.as_str()),
                    entry.get("type").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                let target = local.join(safe_local_name(entry_name));
                match kind {
                    "dir" => folders.push((path.to_string(), target)),
                    "file" | "link-file" => items.push(Item {
                        remote: path.to_string(),
                        local: target,
                        size: entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    }),
                    _ => skipped += 1,
                }
                if items.len() > MAX_FILES {
                    return Err(format!("{name} has more than {MAX_FILES} files"));
                }
            }
        }
        for (_, dir) in &folders {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        self.label = format!("downloading {name} ({} files)", items.len());
        self.total = items.iter().map(|i| i.size).sum();
        for item in &items {
            // Two remote names can clean to the same local one (or differ
            // only in case on a case-insensitive disk): number the second.
            let dest = match (
                item.local.parent(),
                item.local.file_name().and_then(|n| n.to_str()),
            ) {
                (Some(dir), Some(name)) => unique_path(dir, name),
                _ => item.local.clone(),
            };
            self.fetch(&item.remote, &dest).await?;
        }
        Ok(TransferMsg::Finished {
            space: self.space.clone(),
            note: format!(
                "saved {} files to {}{}",
                items.len(),
                root.display(),
                skipped_note(skipped)
            ),
            relist: None,
            open: None,
        })
    }

    async fn upload(
        &mut self,
        local: &Path,
        remote_dir: &str,
        overwrite: bool,
    ) -> Result<TransferMsg, String> {
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} has no file name", local.display()))?
            .to_string();
        let meta = tokio::fs::metadata(local)
            .await
            .map_err(|e| format!("cannot read {}: {e}", local.display()))?;
        let dest = remote_join(remote_dir, &name);
        let space = self.space.clone();
        let done = |note: String| TransferMsg::Finished {
            space: space.clone(),
            note,
            relist: Some(remote_dir.to_string()),
            open: None,
        };
        if meta.is_file() {
            self.label = format!("uploading {name}");
            self.total = meta.len();
            self.send(local, &dest, overwrite).await?;
            return Ok(done(format!("uploaded {name}")));
        }
        if !meta.is_dir() {
            return Err(format!("{} is not a file or folder", local.display()));
        }
        self.label = format!("reading {name}");
        let (dirs, files, skipped) = walk_local(local).await?;
        // The folder itself, then every folder under it, parents first. An
        // existing one is fine when merging (`overwrite`), and only then.
        for rel in std::iter::once(PathBuf::new()).chain(dirs) {
            let path = remote_join_rel(&dest, &rel);
            if let Err(e) = self
                .call("fs.mkdir", serde_json::json!({ "path": path }))
                .await
            {
                let exists = e.contains("already exists");
                if !(exists && (overwrite || !rel.as_os_str().is_empty())) {
                    return Err(e);
                }
            }
        }
        self.label = format!("uploading {name} ({} files)", files.len());
        self.total = files.iter().map(|(_, size)| size).sum();
        let count = files.len();
        for (rel, _) in files {
            self.send(&local.join(&rel), &remote_join_rel(&dest, &rel), overwrite)
                .await?;
        }
        Ok(done(format!(
            "uploaded {name} ({count} files){}",
            skipped_note(skipped)
        )))
    }
}

impl Run {
    async fn upload_many(
        &mut self,
        locals: &[PathBuf],
        remote_dir: &str,
        overwrite: bool,
    ) -> Result<TransferMsg, String> {
        for local in locals {
            self.done = 0;
            self.upload(local, remote_dir, overwrite).await?;
        }
        Ok(TransferMsg::Finished {
            space: self.space.clone(),
            note: format!("uploaded {} items", locals.len()),
            relist: Some(remote_dir.to_string()),
            open: None,
        })
    }
}

fn skipped_note(skipped: usize) -> String {
    match skipped {
        0 => String::new(),
        1 => "; skipped 1 link or special file".into(),
        n => format!("; skipped {n} links or special files"),
    }
}

/// Every folder and file under `root`, as paths relative to it: folders
/// parents-first, files with their sizes. Symlinks and anything that is
/// neither a file nor a folder are counted and skipped.
async fn walk_local(root: &Path) -> Result<(Vec<PathBuf>, Vec<(PathBuf, u64)>, usize), String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut skipped = 0;
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let mut read = tokio::fs::read_dir(root.join(&rel))
            .await
            .map_err(|e| format!("cannot read {}: {e}", root.join(&rel).display()))?;
        while let Some(entry) = read.next_entry().await.map_err(|e| e.to_string())? {
            let child = rel.join(entry.file_name());
            let meta = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                skipped += 1;
            } else if meta.is_dir() {
                dirs.push(child.clone());
                pending.push(child);
            } else if meta.is_file() {
                files.push((child, meta.len()));
                if files.len() > MAX_FILES {
                    return Err(format!(
                        "{} has more than {MAX_FILES} files",
                        root.display()
                    ));
                }
            } else {
                skipped += 1;
            }
        }
    }
    // Shallow paths first, so each folder is made before what goes in it.
    dirs.sort_by_key(|d| d.components().count());
    Ok((dirs, files, skipped))
}

/// `base` joined with a relative local path, in the remote's separators.
fn remote_join_rel(base: &str, rel: &Path) -> String {
    rel.components().fold(base.to_string(), |acc, c| {
        remote_join(&acc, &c.as_os_str().to_string_lossy())
    })
}

/// Fills `buf` as far as the file allows; short only at the end.
async fn read_full(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// `dir` joined with `name` in the remote's own separator style.
pub fn remote_join(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') && !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    if dir.ends_with(sep) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

/// The last component of a remote path, made safe as a local file name on
/// any OS: no separators or reserved characters, no Windows device names,
/// no trailing dots or spaces, never empty.
pub fn safe_local_name(remote: &str) -> String {
    let base = remote.rsplit(['/', '\\']).next().unwrap_or(remote);
    let mut name: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(200)
        .collect();
    while name.ends_with('.') || name.ends_with(' ') {
        name.pop();
    }
    if name.is_empty() || name == "." || name == ".." {
        return "download".to_string();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name.insert(0, '_');
    }
    name
}

/// `dir/name`, or `dir/stem (n).ext` for the first `n` that is free.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// The user's Downloads folder, or their home when there is none.
fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
    }
}

/// A fresh private folder for a file about to be opened.
fn open_dir() -> Result<PathBuf, String> {
    // Unpredictable, created fresh and (on Unix) owner-only, directly in the
    // temp folder — not under a shared folder another account could make
    // first, or a name it could guess and plant a link at.
    let dir =
        std::env::temp_dir().join(format!("spaces-open-{:x}{:x}", random_u64(), random_u64()));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// 64 random bits from the standard library's per-process hash keys — not
/// for cryptography, only so a temporary name cannot be guessed ahead.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish()
}

/// Opens `path` in its default app — only for the image and PDF types the
/// Files tab offers to open, checked again here so nothing else can reach
/// a launcher. No shell is involved: the path is a single argument.
pub fn open_locally(path: &Path) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !crate::files::opens_locally(&ext) {
        return Err(format!("{} is not a type this opens", path.display()));
    }
    let program = if cfg!(target_os = "windows") {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(path)
        // The viewer has no business with this client's credentials.
        .env_remove("SPACES_TOKEN")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot open {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_are_safe_here() {
        assert_eq!(safe_local_name("/srv/app/report.pdf"), "report.pdf");
        assert_eq!(safe_local_name("C:\\x\\a:b|c?.txt"), "a_b_c_.txt");
        assert_eq!(safe_local_name("/x/evil\u{1b}[2J.txt"), "evil_[2J.txt");
        assert_eq!(safe_local_name("/x/CON.txt"), "_CON.txt");
        assert_eq!(safe_local_name("/x/com1"), "_com1");
        assert_eq!(safe_local_name("/x/name. . "), "name");
        assert_eq!(safe_local_name("/x/.."), "download");
        assert_eq!(safe_local_name("/"), "download");
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let dir = std::env::temp_dir().join(format!("spaces-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("a (1).txt"), "x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remote_paths_join_in_their_own_style() {
        assert_eq!(remote_join("/srv/app", "x"), "/srv/app/x");
        assert_eq!(remote_join("/srv/app/", "x"), "/srv/app/x");
        assert_eq!(remote_join("C:\\work", "x"), "C:\\work\\x");
    }

    #[tokio::test]
    async fn a_local_folder_walks_parents_first_and_skips_links() {
        let root = std::env::temp_dir().join(format!("spaces-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        std::fs::write(root.join("top.txt"), "12").unwrap();
        std::fs::write(root.join("a").join("b").join("deep.txt"), "123").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let (dirs, mut files, skipped) = walk_local(&root).await.unwrap();
        assert_eq!(dirs, vec![PathBuf::from("a"), Path::new("a").join("b")]);
        files.sort();
        assert_eq!(
            files,
            vec![
                (Path::new("a").join("b").join("deep.txt"), 3),
                (PathBuf::from("top.txt"), 2)
            ]
        );
        assert_eq!(skipped, usize::from(cfg!(unix)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nested_paths_join_in_the_remotes_style() {
        let rel = Path::new("a").join("b.txt");
        assert_eq!(remote_join_rel("/srv/up", &rel), "/srv/up/a/b.txt");
        assert_eq!(remote_join_rel(r"C:\up", &rel), r"C:\up\a\b.txt");
        assert_eq!(remote_join_rel("/srv/up", Path::new("")), "/srv/up");
    }

    #[test]
    fn only_images_and_pdfs_are_ever_opened() {
        for bad in ["run.exe", "x.bat", "y.lnk", "z.ps1", "doc.html", "noext"] {
            assert!(open_locally(Path::new(bad)).is_err(), "{bad}");
        }
    }
}
