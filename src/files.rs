//! UI-only workspace tracking. No file events are added to model context.
//!
//! All filesystem work runs in spawn_blocking. One bounded watcher/processor is
//! shared by sessions with the same canonical cwd. Limits are deliberately
//! explicit: oversized workspaces fail rather than silently missing turn files.
use crate::store::Store;
use anyhow::{Context, Result, anyhow, ensure};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::sync::{Mutex, mpsc, oneshot};

const MAX_PREVIEW: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
const MAX_DIRS: usize = 4096;
const MAX_DEPTH: usize = 64;
const MAX_WORKSPACES: usize = 32;
const MAX_SESSIONS: usize = 1024;
const QUEUE: usize = 512;
const MAX_PENDING: usize = 4096;
const EMIT_BATCH: usize = 128;
const DEBOUNCE: Duration = Duration::from_millis(150);
const IGNORED: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".runtime",
    ".test-artifacts",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
];

type Snapshot = BTreeMap<String, Stamp>;
type Changes = BTreeMap<String, Change>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
    // ctime and inode catch same-size rewrites/atomic replacements without hashes.
    inode: u64,
    ctime: (i64, i64),
}
impl Stamp {
    fn from_meta(meta: &Metadata) -> Self {
        Self {
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified: meta.modified().ok(),
            inode: meta.ino(),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}
#[derive(Clone, Debug)]
struct Change {
    kind: &'static str,
    is_dir: bool,
}
struct Turn {
    run_id: String,
    before: Snapshot,
    changes: Changes,
}
#[derive(Default)]
struct SessionState {
    turn: Option<Turn>,
    statuses: BTreeMap<String, &'static str>,
}
struct Workspace {
    tx: mpsc::Sender<Command>,
}
enum Command {
    Subscribe {
        id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Begin {
        id: String,
        run_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Finish {
        id: String,
        run_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Status {
        id: String,
        reply: oneshot::Sender<BTreeMap<String, &'static str>>,
    },
}
struct WatchState {
    root: PathBuf,
    private_root: PathBuf,
    watcher: RecommendedWatcher,
    watched: BTreeSet<String>,
    known: Snapshot,
}

#[derive(Clone)]
pub struct FileTracker {
    store: Store,
    private_root: PathBuf,
    workspaces: Arc<Mutex<HashMap<PathBuf, Arc<Workspace>>>>,
    sessions: Arc<Mutex<HashMap<String, PathBuf>>>,
}
impl FileTracker {
    pub fn new(store: Store) -> Self {
        Self {
            private_root: store.data_dir().to_owned(),
            store,
            workspaces: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn watch(&self, id: &str, cwd: &str) -> Result<()> {
        let root = canonical_root_async(cwd).await?;
        ensure!(
            !root.starts_with(&self.private_root),
            "Private application data is not a browsable workspace"
        );
        if let Some(existing) = self.sessions.lock().await.get(id) {
            ensure!(
                *existing == root,
                "Session workspace cannot change while watched"
            );
        }
        let workspace = {
            // Serialize installation only, never hold this lock during event IO.
            let mut workspaces = self.workspaces.lock().await;
            if let Some(workspace) = workspaces.get(&root) {
                workspace.clone()
            } else {
                ensure!(
                    workspaces.len() < MAX_WORKSPACES,
                    "File watcher workspace limit reached ({MAX_WORKSPACES})"
                );
                let (notify_tx, notify_rx) = mpsc::channel(QUEUE);
                let overflow = Arc::new(AtomicBool::new(false));
                let flag = overflow.clone();
                let setup_root = root.clone();
                let private_root = self.private_root.clone();
                let callback_private = private_root.clone();
                let state = blocking(move || {
                    let watcher =
                        notify::recommended_watcher(move |mut event: notify::Result<Event>| {
                            if let Ok(event) = &mut event {
                                // Never let our own SQLite/JSONL writes feed the
                                // UI event log and recursively trigger this watcher.
                                event
                                    .paths
                                    .retain(|path| !path.starts_with(&callback_private));
                                if event.paths.is_empty() {
                                    return;
                                }
                            }
                            // Filter before queueing: metadata scans themselves cause
                            // Access notifications; queueing those could self-trigger
                            // endless overflow/rescan cycles in a large workspace.
                            if event.as_ref().is_ok_and(|event| !is_change(event)) {
                                return;
                            }
                            // notify's thread must never block on SQLite or an async lock.
                            if notify_tx.try_send(event).is_err() {
                                flag.store(true, Ordering::Release);
                            }
                        })?;
                    let mut state = WatchState {
                        root: setup_root,
                        private_root,
                        watcher,
                        watched: BTreeSet::new(),
                        known: Snapshot::new(),
                    };
                    state
                        .watcher
                        .watch(&state.root, RecursiveMode::NonRecursive)?;
                    state.watched.insert(String::new());
                    state.refresh()?;
                    Ok(state)
                })
                .await?;
                let (tx, rx) = mpsc::channel(64);
                tokio::spawn(processor(
                    self.store.clone(),
                    state,
                    rx,
                    notify_rx,
                    overflow,
                ));
                let workspace = Arc::new(Workspace { tx });
                workspaces.insert(root.clone(), workspace.clone());
                workspace
            }
        };
        let (reply, wait) = oneshot::channel();
        workspace
            .tx
            .send(Command::Subscribe {
                id: id.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("File watcher stopped"))?;
        wait.await.context("File watcher stopped")??;
        self.sessions.lock().await.insert(id.into(), root);
        Ok(())
    }

    pub async fn begin_turn(&self, id: &str, cwd: &str, run_id: &str) -> Result<()> {
        self.watch(id, cwd).await?;
        let workspace = self.workspace(id).await?;
        let (reply, wait) = oneshot::channel();
        workspace
            .tx
            .send(Command::Begin {
                id: id.into(),
                run_id: run_id.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("File watcher stopped"))?;
        wait.await.context("File watcher stopped")?
    }

    pub async fn finish_turn(&self, id: &str, run_id: &str) -> Result<()> {
        let workspace = self.workspace(id).await?;
        let (reply, wait) = oneshot::channel();
        workspace
            .tx
            .send(Command::Finish {
                id: id.into(),
                run_id: run_id.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("File watcher stopped"))?;
        wait.await.context("File watcher stopped")?
    }

    async fn workspace(&self, id: &str) -> Result<Arc<Workspace>> {
        let root = self
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .context("Session has no file watcher")?;
        self.workspaces
            .lock()
            .await
            .get(&root)
            .cloned()
            .context("File watcher unavailable")
    }

    pub async fn list(&self, id: &str, cwd: &str, path: &str) -> Result<Value> {
        // Validate before watching so a rejected path cannot start resource work.
        relative_parts(path)?;
        self.watch(id, cwd).await?;
        let workspace = self.workspace(id).await?;
        let (reply, wait) = oneshot::channel();
        workspace
            .tx
            .send(Command::Status {
                id: id.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow!("File watcher stopped"))?;
        let statuses = wait.await.context("File watcher stopped")?;
        let cwd = cwd.to_owned();
        let path = path.to_owned();
        let private_root = self.private_root.clone();
        blocking(move || list_directory_excluding(&cwd, &path, &statuses, Some(&private_root)))
            .await
    }

    /// HTTP owner MUST set CSP sandbox (without allow-same-origin/scripts),
    /// nosniff, no-store, and existing cookie authentication on these bytes.
    pub async fn preview(&self, cwd: &str, path: &str) -> Result<(Vec<u8>, String)> {
        let cwd = cwd.to_owned();
        let path = path.to_owned();
        let private_root = self.private_root.clone();
        blocking(move || preview_file_excluding(&cwd, &path, Some(&private_root))).await
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("File worker failed")?
}
async fn canonical_root_async(cwd: &str) -> Result<PathBuf> {
    let cwd = cwd.to_owned();
    blocking(move || canonical_root(&cwd)).await
}
fn canonical_root(cwd: &str) -> Result<PathBuf> {
    let root = fs::canonicalize(cwd).context("Cannot resolve workspace")?;
    ensure!(root.is_dir(), "Workspace is not a directory");
    Ok(root)
}
fn relative_parts(path: &str) -> Result<Vec<&str>> {
    ensure!(
        !path.contains('\\') && !path.contains('\0'),
        "Invalid relative path"
    );
    ensure!(
        !Path::new(path).is_absolute(),
        "Absolute paths are forbidden"
    );
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "Invalid relative path"
        );
        ensure!(
            !IGNORED.contains(&part),
            "Private/vendor directory is not browsable"
        );
        ensure!(!part.contains(':'), "Invalid relative path");
        parts.push(part);
    }
    ensure!(parts.len() <= MAX_DEPTH, "Path is too deep");
    Ok(parts)
}
fn visible_relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        let part = part.to_str()?;
        if IGNORED.contains(&part) || part.contains('\\') || part.contains(':') {
            return None;
        }
        parts.push(part);
    }
    if parts.len() > MAX_DEPTH {
        return None;
    }
    Some(parts.join("/"))
}

/// Anchor every open at a directory descriptor, refusing *all* symlinks, not
/// only out-of-root links. O_NONBLOCK avoids hanging on FIFOs/devices before
/// fstat rejects them. Unlike canonicalize-then-read, this resists link swaps.
fn open_relative(root: &Path, path: &str, private_root: Option<&Path>) -> Result<File> {
    let parts = relative_parts(path)?;
    ensure!(
        !private_root.is_some_and(|private| root.join(path).starts_with(private)),
        "Private application data is not browsable"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(root)
        .context("Cannot open workspace")?;
    for (index, part) in parts.iter().enumerate() {
        use nix::{
            fcntl::{OFlag, openat},
            sys::stat::Mode,
        };
        let mut flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
        if index + 1 < parts.len() {
            flags |= OFlag::O_DIRECTORY;
        }
        let fd = openat(&file, *part, flags, Mode::empty())
            .context("Cannot open workspace path (symlinks are not allowed)")?;
        file = File::from(fd);
        let meta = file.metadata()?;
        ensure!(
            meta.is_file() || meta.is_dir(),
            "Special files are not browsable"
        );
    }
    // A concurrently moved directory must not turn an anchored descriptor into
    // a read outside the canonical workspace. Linux /proc exposes actual fds.
    let actual = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    ensure!(actual.starts_with(root), "Path escaped workspace");
    ensure!(
        !private_root.is_some_and(|private| actual.starts_with(private)),
        "Private application data is not browsable"
    );
    Ok(file)
}
fn directory_entries(dir: &File) -> Result<fs::ReadDir> {
    ensure!(dir.metadata()?.is_dir(), "Path is not a directory");
    // Descriptor stays alive while the caller consumes this iterator.
    Ok(fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?)
}
fn list_directory_excluding(
    cwd: &str,
    path: &str,
    statuses: &BTreeMap<String, &'static str>,
    private_root: Option<&Path>,
) -> Result<Value> {
    let root = canonical_root(cwd)?;
    let directory = open_relative(&root, path, private_root)?;
    let mut entries = Vec::new();
    for entry in directory_entries(&directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let relative = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        if relative_parts(&relative).is_err() {
            continue;
        }
        let kind = entry.file_type()?;
        if !kind.is_dir() && !kind.is_file() {
            continue;
        }
        // Open each entry safely too, including concurrent replacements.
        let Ok(file) = open_relative(&root, &relative, private_root) else {
            continue;
        };
        let meta = file.metadata()?;
        let mut value =
            json!({"path":relative,"name":name,"is_dir":meta.is_dir(),"size":meta.len()});
        if let Ok(modified) = meta.modified() {
            let time: chrono::DateTime<chrono::Utc> = modified.into();
            value["modified_at"] = json!(time.to_rfc3339());
        }
        if let Some(status) = statuses.get(&relative) {
            value["status"] = json!(status);
        }
        entries.push(value);
        ensure!(
            entries.len() <= MAX_ENTRIES,
            "Directory entry limit reached"
        );
    }
    entries.sort_by(|a, b| {
        b["is_dir"]
            .as_bool()
            .cmp(&a["is_dir"].as_bool())
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    Ok(json!({"path":path,"entries":entries}))
}
fn preview_file_excluding(
    cwd: &str,
    path: &str,
    private_root: Option<&Path>,
) -> Result<(Vec<u8>, String)> {
    ensure!(!path.is_empty(), "A file path is required");
    let root = canonical_root(cwd)?;
    let mut file = open_relative(&root, path, private_root)?;
    let meta = file.metadata()?;
    ensure!(meta.is_file(), "Preview requires a regular file");
    ensure!(meta.len() <= MAX_PREVIEW, "Preview exceeds 32 MiB limit");
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&mut file).take(MAX_PREVIEW + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_PREVIEW,
        "Preview exceeds 32 MiB limit"
    );
    let extension = Path::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        // Never execute source, XML, PDF or an unknown binary under a guessed
        // active MIME. Text is explicitly plain; binary is a download.
        _ if !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok() => {
            "text/plain; charset=utf-8"
        }
        _ => "application/octet-stream",
    };
    Ok((bytes, mime.into()))
}

impl WatchState {
    fn refresh(&mut self) -> Result<Changes> {
        let mut snapshot = scan(&self.root, Some(&self.private_root))?;
        let dirs: BTreeSet<_> = std::iter::once(String::new())
            .chain(
                snapshot
                    .iter()
                    .filter(|(_, stamp)| stamp.is_dir)
                    .map(|(path, _)| path.clone()),
            )
            .collect();
        for path in self.watched.difference(&dirs) {
            let _ = self.watcher.unwatch(&self.root.join(path));
        }
        let added = dirs.difference(&self.watched).next().is_some();
        for path in dirs.difference(&self.watched) {
            self.watcher
                .watch(&self.root.join(path), RecursiveMode::NonRecursive)
                .with_context(|| format!("Cannot watch directory {path}"))?;
        }
        self.watched = dirs;
        if added {
            // Bridge the gap between discovering a new subtree and installing
            // its watches. No content hashes and no per-token recursive scans.
            snapshot = scan(&self.root, Some(&self.private_root))?;
        }
        let changes = difference(&self.known, &snapshot);
        self.known = snapshot;
        Ok(changes)
    }

    fn batch(&mut self, events: Vec<Event>, overflow: bool) -> Result<Changes> {
        if overflow {
            return self.refresh();
        }
        let mut paths = BTreeSet::new();
        let mut needs_refresh = false;
        for event in events {
            for absolute in event.paths {
                if absolute.starts_with(&self.private_root) {
                    continue;
                }
                let Some(path) = visible_relative(&self.root, &absolute) else {
                    continue;
                };
                if path.is_empty() {
                    continue;
                }
                if self.known.get(&path).is_some_and(|s| s.is_dir) {
                    needs_refresh = true;
                } else if let Ok(file) = open_relative(&self.root, &path, Some(&self.private_root))
                    && file.metadata()?.is_dir()
                {
                    needs_refresh = true;
                }
                paths.insert(path);
            }
        }
        // Directory creation/removal/rename needs watch maintenance and subtree
        // discovery. Ordinary file writes inspect only the affected files.
        if needs_refresh {
            return self.refresh();
        }
        let mut changes = Changes::new();
        for path in paths {
            match open_relative(&self.root, &path, Some(&self.private_root))
                .and_then(|file| Ok(file.metadata()?))
            {
                Ok(meta) if meta.is_file() => {
                    let stamp = Stamp::from_meta(&meta);
                    let old = self.known.get(&path);
                    // A begin/end snapshot may already have consumed this
                    // notify event. Do not leak delayed events into next turn.
                    if old == Some(&stamp) {
                        continue;
                    }
                    let kind = if old.is_some() { "modified" } else { "new" };
                    ensure!(
                        old.is_some() || self.known.len() < MAX_ENTRIES,
                        "Workspace exceeds {MAX_ENTRIES} tracked entries"
                    );
                    self.known.insert(path.clone(), stamp);
                    changes.insert(
                        path,
                        Change {
                            kind,
                            is_dir: false,
                        },
                    );
                }
                _ => {
                    if let Some(old) = self.known.remove(&path) {
                        changes.insert(
                            path,
                            Change {
                                kind: "deleted",
                                is_dir: old.is_dir,
                            },
                        );
                    }
                }
            }
        }
        Ok(changes)
    }
}
fn scan(root: &Path, private_root: Option<&Path>) -> Result<Snapshot> {
    let mut result = Snapshot::new();
    let mut pending = vec![String::new()];
    let mut dirs = 0usize;
    while let Some(path) = pending.pop() {
        dirs += 1;
        ensure!(
            dirs <= MAX_DIRS,
            "Workspace exceeds {MAX_DIRS} watched directories"
        );
        let directory = match open_relative(root, &path, private_root) {
            Ok(directory) => directory,
            Err(_) if !path.is_empty() => continue, // concurrent removal or inaccessible subtree
            Err(error) => return Err(error),
        };
        for entry in directory_entries(&directory)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let relative = if path.is_empty() {
                name
            } else {
                format!("{path}/{name}")
            };
            if relative_parts(&relative).is_err() {
                continue;
            }
            let kind = entry.file_type()?;
            if !kind.is_file() && !kind.is_dir() {
                continue;
            }
            let Ok(file) = open_relative(root, &relative, private_root) else {
                continue;
            };
            let meta = file.metadata()?;
            if meta.is_dir() {
                pending.push(relative.clone());
            }
            result.insert(relative, Stamp::from_meta(&meta));
            ensure!(
                result.len() <= MAX_ENTRIES,
                "Workspace exceeds {MAX_ENTRIES} tracked entries"
            );
        }
    }
    Ok(result)
}
fn difference(before: &Snapshot, after: &Snapshot) -> Changes {
    let mut result = Changes::new();
    for (path, stamp) in after {
        match before.get(path) {
            None => {
                result.insert(
                    path.clone(),
                    Change {
                        kind: "new",
                        is_dir: stamp.is_dir,
                    },
                );
            }
            Some(old) if old.is_dir != stamp.is_dir || (!stamp.is_dir && old != stamp) => {
                result.insert(
                    path.clone(),
                    Change {
                        kind: "modified",
                        is_dir: stamp.is_dir,
                    },
                );
            }
            _ => {}
        }
    }
    for (path, stamp) in before {
        if !after.contains_key(path) {
            result.insert(
                path.clone(),
                Change {
                    kind: "deleted",
                    is_dir: stamp.is_dir,
                },
            );
        }
    }
    result
}
fn change_value(path: &str, change: &Change) -> Value {
    json!({"path":path,"name":path.rsplit('/').next().unwrap_or(path),"kind":change.kind,"is_dir":change.is_dir})
}
fn accumulate(
    sessions: &mut HashMap<String, SessionState>,
    pending: &mut Changes,
    changes: Changes,
) {
    for (path, change) in changes {
        for session in sessions.values_mut() {
            if let Some(turn) = &mut session.turn {
                let kind = if change.kind == "deleted" {
                    "deleted"
                } else if turn.before.contains_key(&path) {
                    "modified"
                } else {
                    "new"
                };
                if turn.changes.len() < MAX_ENTRIES || turn.changes.contains_key(&path) {
                    turn.changes.insert(
                        path.clone(),
                        Change {
                            kind,
                            is_dir: change.is_dir,
                        },
                    );
                }
                if !change.is_dir {
                    if kind == "deleted" {
                        session.statuses.remove(&path);
                    } else if session.statuses.len() < MAX_ENTRIES {
                        session.statuses.insert(path.clone(), kind);
                    }
                }
            }
        }
        if pending.len() < MAX_PENDING || pending.contains_key(&path) {
            pending.insert(path, change);
        } else {
            // A root invalidation keeps lazy trees fresh when a mass rewrite
            // exceeds the UI queue; active turn snapshots remain complete.
            pending.insert(
                String::new(),
                Change {
                    kind: "modified",
                    is_dir: true,
                },
            );
        }
    }
}
async fn emit(store: &Store, sessions: &HashMap<String, SessionState>, pending: &mut Changes) {
    for _ in 0..EMIT_BATCH {
        let Some((path, change)) = pending.pop_first() else {
            break;
        };
        for id in sessions.keys() {
            if let Err(error) = store
                .append_event(id, "file_changed", change_value(&path, &change))
                .await
            {
                tracing::warn!(session = %id, %error, "Cannot persist file change");
            }
        }
    }
}
async fn refresh_async(state: Arc<std::sync::Mutex<WatchState>>) -> Result<(Changes, Snapshot)> {
    blocking(move || {
        let mut state = state
            .lock()
            .map_err(|_| anyhow!("File watcher lock poisoned"))?;
        let changes = state.refresh()?;
        Ok((changes, state.known.clone()))
    })
    .await
}
fn is_change(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(
                notify::event::ModifyKind::Data(_)
                    | notify::event::ModifyKind::Name(_)
                    | notify::event::ModifyKind::Any
            )
            | EventKind::Any
    )
}

async fn processor(
    store: Store,
    state: WatchState,
    mut commands: mpsc::Receiver<Command>,
    mut notifications: mpsc::Receiver<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
) {
    let state = Arc::new(std::sync::Mutex::new(state));
    let mut sessions: HashMap<String, SessionState> = HashMap::new();
    let mut events = Vec::new();
    let mut pending = Changes::new();
    let mut interval = tokio::time::interval(DEBOUNCE);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break; };
                match command {
                    Command::Subscribe { id, reply } => {
                        let result = if sessions.contains_key(&id) { Ok(()) }
                            else if sessions.len() >= MAX_SESSIONS { Err(anyhow!("Workspace session watcher limit reached")) }
                            else { sessions.insert(id, SessionState::default()); Ok(()) };
                        let _ = reply.send(result);
                    }
                    Command::Status { id, reply } => {
                        let _ = reply.send(sessions.get(&id).map(|s| s.statuses.clone()).unwrap_or_default());
                    }
                    Command::Begin { id, run_id, reply } => {
                        let result = async {
                            ensure!(sessions.get(&id).is_some_and(|s| s.turn.is_none()), "Session already has an active file turn");
                            let (changes, before) = refresh_async(state.clone()).await?;
                            accumulate(&mut sessions, &mut pending, changes);
                            let session = sessions.get_mut(&id).context("Session is not subscribed")?;
                            session.statuses.clear();
                            session.turn = Some(Turn { run_id, before, changes: Changes::new() });
                            Ok(())
                        }.await;
                        let _ = reply.send(result);
                    }
                    Command::Finish { id, run_id, reply } => {
                        let result = async {
                            ensure!(sessions.get(&id).and_then(|s| s.turn.as_ref()).is_some_and(|t| t.run_id == run_id), "No matching active file turn");
                            // End snapshot is a completion barrier independent of notify
                            // delivery latency, including write(); finish_turn() immediately.
                            let (after, scan_error) = match refresh_async(state.clone()).await {
                                Ok((changes, after)) => {
                                    accumulate(&mut sessions, &mut pending, changes);
                                    (Some(after), None)
                                }
                                Err(error) => (None, Some(error)),
                            };
                            let turn = sessions.get_mut(&id).and_then(|s| s.turn.as_mut()).unwrap();
                            if let Some(after) = after {
                                for (path, change) in difference(&turn.before, &after) {
                                    turn.changes.insert(path, change);
                                }
                            }
                            let files: Vec<Value> = turn.changes.iter().filter(|(_, c)| !c.is_dir)
                                .map(|(path, change)| {
                                    let mut value = change_value(path, change);
                                    value.as_object_mut().unwrap().remove("is_dir");
                                    value
                                }).collect();
                            store.append_event(&id, "turn_files", json!({"run_id":run_id,"files":files})).await?;
                            sessions.get_mut(&id).unwrap().turn = None;
                            // Even a removed/unreadable workspace gets its UI-only
                            // completion card; propagate the incomplete-scan error.
                            if let Some(error) = scan_error { return Err(error); }
                            Ok(())
                        }.await;
                        let _ = reply.send(result);
                    }
                }
            }
            notification = notifications.recv() => {
                match notification {
                    Some(Ok(event)) if is_change(&event) => {
                        if events.len() < QUEUE { events.push(event); }
                        else { overflow.store(true, Ordering::Release); }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Workspace notify error; reconciling metadata");
                        overflow.store(true, Ordering::Release);
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = interval.tick() => {
                let rescan = overflow.swap(false, Ordering::AcqRel);
                if rescan || !events.is_empty() {
                    let batch = std::mem::take(&mut events);
                    let state = state.clone();
                    let result = blocking(move || state.lock().map_err(|_| anyhow!("File watcher lock poisoned"))?.batch(batch, rescan)).await;
                    match result {
                        Ok(changes) => accumulate(&mut sessions, &mut pending, changes),
                        Err(error) => tracing::warn!(%error, "Cannot reconcile workspace changes"),
                    }
                }
                emit(&store, &sessions, &mut pending).await;
            }
        }
    }
    // notify may join its native thread when dropped; do that off the runtime.
    let _ = tokio::task::spawn_blocking(move || drop(state)).await;
}

#[cfg(test)]
mod tests {
    fn list_directory(
        cwd: &str,
        path: &str,
        statuses: &BTreeMap<String, &'static str>,
    ) -> Result<Value> {
        list_directory_excluding(cwd, path, statuses, None)
    }
    fn preview_file(cwd: &str, path: &str) -> Result<(Vec<u8>, String)> {
        preview_file_excluding(cwd, path, None)
    }
    use super::*;
    use crate::models::Selection;
    use std::os::unix::fs::symlink;

    #[test]
    fn traversal_absolute_and_private_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("ok.txt"), "ok").unwrap();
        let cwd = root.path().to_str().unwrap();
        for path in [
            "../outside",
            "/etc/passwd",
            "a/../../outside",
            "a/../ok.txt",
            "./ok.txt",
            "a//b",
            "a\\b",
            ".runtime/secret",
            "node_modules/pkg",
            "a/.git/config",
            "C:/secret",
        ] {
            assert!(
                list_directory(cwd, path, &BTreeMap::new()).is_err(),
                "accepted list {path}"
            );
            assert!(preview_file(cwd, path).is_err(), "accepted preview {path}");
        }
        assert_eq!(preview_file(cwd, "ok.txt").unwrap().0, b"ok");
    }

    #[test]
    fn lazy_listing_is_sorted_and_skips_symlinks_and_private_dirs() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("z-dir")).unwrap();
        fs::write(root.path().join("a.txt"), "a").unwrap();
        fs::write(root.path().join(".env.example"), "x").unwrap();
        fs::create_dir(root.path().join(".runtime")).unwrap();
        symlink(root.path().join("a.txt"), root.path().join("link")).unwrap();
        let listing = list_directory(root.path().to_str().unwrap(), "", &BTreeMap::new()).unwrap();
        let entries = listing["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["name"], "z-dir");
        assert!(
            entries
                .iter()
                .all(|v| v["name"] != ".runtime" && v["name"] != "link")
        );
        assert!(list_directory(root.path().to_str().unwrap(), "z-dir", &BTreeMap::new()).unwrap()["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn symlink_escape_and_internal_symlinks_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "not allowed").unwrap();
        fs::write(root.path().join("inside"), "inside").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        symlink(
            outside.path().join("secret"),
            root.path().join("secret-link"),
        )
        .unwrap();
        symlink(
            root.path().join("inside"),
            root.path().join("internal-link"),
        )
        .unwrap();
        let cwd = root.path().to_str().unwrap();
        for path in ["escape/secret", "secret-link", "internal-link"] {
            assert!(preview_file(cwd, path).is_err());
        }
        assert!(list_directory(cwd, "escape", &BTreeMap::new()).is_err());
        let snapshot = scan(root.path(), None).unwrap();
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.contains_key("inside"));
    }

    #[test]
    fn preview_mime_size_and_special_file_rejection() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().to_str().unwrap();
        for (name, bytes, expected) in [
            (
                "page.HTML",
                &b"<html>preview</html>"[..],
                "text/html; charset=utf-8",
            ),
            ("drawing.svg", &b"<svg/>"[..], "image/svg+xml"),
            ("a.png", &b"\x89PNG\0"[..], "image/png"),
            ("a.jpg", &b"\xff\xd8\xff"[..], "image/jpeg"),
            ("a.webp", &b"RIFF\0WEBP"[..], "image/webp"),
            ("a.gif", &b"GIF89a"[..], "image/gif"),
            ("a.js", &b"alert(1)"[..], "text/plain; charset=utf-8"),
            ("a.xml", &b"<xml/>"[..], "text/plain; charset=utf-8"),
            ("a.bin", &b"\0\xff"[..], "application/octet-stream"),
        ] {
            fs::write(root.path().join(name), bytes).unwrap();
            let (actual, mime) = preview_file(cwd, name).unwrap();
            assert_eq!(actual, bytes);
            assert_eq!(mime, expected);
        }
        let large = File::create(root.path().join("large")).unwrap();
        large.set_len(MAX_PREVIEW + 1).unwrap();
        assert!(preview_file(cwd, "large").is_err());
        assert!(preview_file(cwd, "").is_err());
        fs::create_dir(root.path().join("dir")).unwrap();
        assert!(preview_file(cwd, "dir").is_err());
        nix::unistd::mkfifo(
            &root.path().join("fifo"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(preview_file(cwd, "fifo").is_err());
        assert!(
            !list_directory(cwd, "", &BTreeMap::new()).unwrap()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "fifo")
        );
    }

    async fn setup() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Store,
        FileTracker,
        String,
    ) {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&data.path().join("test.sqlite")).unwrap();
        let id = add_session(&store, root.path()).await;
        let tracker = FileTracker::new(store.clone());
        (data, root, store, tracker, id)
    }
    async fn add_session(store: &Store, cwd: &Path) -> String {
        store
            .create_session(
                "File test".into(),
                cwd.to_string_lossy().into(),
                Selection {
                    site_id: "site".into(),
                    key_id: "key".into(),
                    model_id: "model".into(),
                },
                None,
                0,
            )
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn actual_private_data_root_is_excluded_without_hiding_normal_data_dirs() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("ordinary").join("data");
        fs::create_dir_all(&private).unwrap();
        fs::create_dir_all(root.path().join("src/data")).unwrap();
        let store = Store::open(&private.join("db.sqlite")).unwrap();
        let id = add_session(&store, root.path()).await;
        let tracker = FileTracker::new(store.clone());
        let cwd = root.path().to_str().unwrap();
        tracker.begin_turn(&id, cwd, "private-test").await.unwrap();
        fs::write(private.join("secret.txt"), "not browsable").unwrap();
        fs::write(root.path().join("src/data/public.txt"), "public").unwrap();
        tracker.finish_turn(&id, "private-test").await.unwrap();
        let entries = tracker.list(&id, cwd, "ordinary").await.unwrap();
        assert_eq!(entries["entries"], json!([]));
        assert!(tracker.list(&id, cwd, "ordinary/data").await.is_err());
        assert!(
            tracker
                .preview(cwd, "ordinary/data/secret.txt")
                .await
                .is_err()
        );
        assert!(
            tracker
                .preview(cwd, "ordinary/data/db.sqlite")
                .await
                .is_err()
        );
        assert_eq!(
            tracker.preview(cwd, "src/data/public.txt").await.unwrap().0,
            b"public"
        );
        assert_eq!(
            tracker.list(&id, cwd, "src/data").await.unwrap()["entries"][0]["name"],
            "public.txt"
        );
        assert!(
            tracker
                .watch("private-root", private.to_str().unwrap())
                .await
                .is_err()
        );
        fs::create_dir(private.join("child")).unwrap();
        assert!(
            tracker
                .watch("private-child", private.join("child").to_str().unwrap())
                .await
                .is_err()
        );
        assert!(
            tracker
                .preview(private.to_str().unwrap(), "secret.txt")
                .await
                .is_err()
        );
        // Permit all legitimate debounce notifications to settle, then write
        // SQLite/JSONL again and prove it does not generate recursive UI events.
        tokio::time::sleep(DEBOUNCE * 3).await;
        let before = store.events(&id, 0).await.unwrap();
        let changed = before
            .iter()
            .filter(|event| event.kind == "file_changed")
            .count();
        store
            .append_event(&id, "test_marker", json!({"test":true}))
            .await
            .unwrap();
        fs::write(private.join("secret.txt"), "still private").unwrap();
        tokio::time::sleep(DEBOUNCE * 3).await;
        let after = store.events(&id, 0).await.unwrap();
        assert_eq!(
            after
                .iter()
                .filter(|event| event.kind == "file_changed")
                .count(),
            changed
        );
        assert!(
            after
                .iter()
                .filter(|event| event.kind == "file_changed")
                .all(|event| !event.data["path"]
                    .as_str()
                    .unwrap()
                    .starts_with("ordinary/data"))
        );
        let turn = after
            .iter()
            .find(|event| event.kind == "turn_files")
            .unwrap();
        assert_eq!(
            turn.data["files"],
            json!([{"path":"src/data/public.txt","name":"public.txt","kind":"new"}])
        );
        // The private subtree is absent from the metadata baseline too.
        let private_path = private.clone();
        let root_path = root.path().to_owned();
        let snapshot = blocking(move || scan(&root_path, Some(&private_path)))
            .await
            .unwrap();
        assert!(
            snapshot
                .keys()
                .all(|path| !path.starts_with("ordinary/data"))
        );
    }

    #[tokio::test]
    async fn immediate_finish_reports_new_modified_deleted_and_empty_turns() {
        let (_data, root, store, tracker, id) = setup().await;
        fs::write(root.path().join("existing"), "before").unwrap();
        fs::write(root.path().join("deleted"), "before").unwrap();
        tracker
            .begin_turn(&id, root.path().to_str().unwrap(), "run-1")
            .await
            .unwrap();
        fs::write(root.path().join("new.html"), "<html/>").unwrap();
        fs::write(root.path().join("existing"), "after!").unwrap();
        fs::remove_file(root.path().join("deleted")).unwrap();
        tracker.finish_turn(&id, "run-1").await.unwrap();
        let events = store.events(&id, 0).await.unwrap();
        let turn = events
            .iter()
            .find(|event| event.kind == "turn_files")
            .unwrap();
        let files = turn.data["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
        for (path, kind) in [
            ("new.html", "new"),
            ("existing", "modified"),
            ("deleted", "deleted"),
        ] {
            assert!(
                files
                    .iter()
                    .any(|file| file["path"] == path && file["kind"] == kind)
            );
        }
        assert!(
            store.context(&id).await.unwrap().is_empty(),
            "UI events must not enter model context"
        );
        tracker
            .begin_turn(&id, root.path().to_str().unwrap(), "run-2")
            .await
            .unwrap();
        tracker.finish_turn(&id, "run-2").await.unwrap();
        let events = store.events(&id, 0).await.unwrap();
        let empty = events
            .iter()
            .find(|event| event.kind == "turn_files" && event.data["run_id"] == "run-2")
            .unwrap();
        assert_eq!(empty.data["files"], json!([]));
        assert!(tracker.finish_turn(&id, "wrong").await.is_err());
    }

    #[tokio::test]
    async fn shared_cwd_external_writes_reach_every_session_and_scoped_turn() {
        let (_data, root, store, tracker, first) = setup().await;
        let second = add_session(&store, root.path()).await;
        let cwd = root.path().to_str().unwrap();
        tracker.watch(&first, cwd).await.unwrap();
        tracker.begin_turn(&second, cwd, "child-run").await.unwrap();
        assert_eq!(tracker.workspaces.lock().await.len(), 1);
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("printf hello > external.txt")
            .current_dir(root.path())
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let a = store.events(&first, 0).await.unwrap();
                let b = store.events(&second, 0).await.unwrap();
                if [&a, &b].iter().all(|events| {
                    events.iter().any(|event| {
                        event.kind == "file_changed" && event.data["path"] == "external.txt"
                    })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("notify must publish external changes without list/finish polling");
        tracker.finish_turn(&second, "child-run").await.unwrap();
        let events = store.events(&second, 0).await.unwrap();
        assert!(
            events.iter().any(|event| event.kind == "turn_files"
                && event.data["files"][0]["path"] == "external.txt")
        );
        assert!(
            store
                .events(&first, 0)
                .await
                .unwrap()
                .iter()
                .all(|event| event.kind != "turn_files")
        );
    }

    #[tokio::test]
    async fn public_api_statuses_preview_and_delayed_events_are_turn_scoped() {
        let (_data, root, store, tracker, id) = setup().await;
        let cwd = root.path().to_str().unwrap();
        tracker.begin_turn(&id, cwd, "first").await.unwrap();
        fs::write(root.path().join("artifact.svg"), "<svg/>").unwrap();
        tracker.finish_turn(&id, "first").await.unwrap();
        let listing = tracker.list(&id, cwd, "").await.unwrap();
        assert_eq!(listing["entries"][0]["status"], "new");
        let (bytes, mime) = tracker.preview(cwd, "artifact.svg").await.unwrap();
        assert_eq!(bytes, b"<svg/>");
        assert_eq!(mime, "image/svg+xml");
        assert!(tracker.list(&id, cwd, "../").await.is_err());
        assert!(tracker.preview(cwd, "../secret").await.is_err());
        tracker.begin_turn(&id, cwd, "second").await.unwrap();
        // Let queued notifications from the first turn pass the debounce tick.
        tokio::time::sleep(DEBOUNCE * 2).await;
        tracker.finish_turn(&id, "second").await.unwrap();
        let events = store.events(&id, 0).await.unwrap();
        let second = events
            .iter()
            .find(|event| event.kind == "turn_files" && event.data["run_id"] == "second")
            .unwrap();
        assert_eq!(second.data["files"], json!([]));
    }

    #[test]
    fn access_and_metadata_events_never_enter_the_bounded_queue() {
        assert!(!is_change(&Event::new(EventKind::Access(
            notify::event::AccessKind::Any
        ))));
        assert!(!is_change(&Event::new(EventKind::Modify(
            notify::event::ModifyKind::Metadata(notify::event::MetadataKind::Any)
        ))));
        assert!(is_change(&Event::new(EventKind::Create(
            notify::event::CreateKind::File
        ))));
    }

    #[tokio::test]
    async fn private_and_vendor_writes_do_not_appear_in_turn_files() {
        let (_data, root, store, tracker, id) = setup().await;
        tracker
            .begin_turn(&id, root.path().to_str().unwrap(), "run")
            .await
            .unwrap();
        for ignored in [
            ".runtime",
            "node_modules",
            "target",
            ".git",
            ".test-artifacts",
        ] {
            fs::create_dir(root.path().join(ignored)).unwrap();
            fs::write(root.path().join(ignored).join("private"), "private").unwrap();
        }
        tracker.finish_turn(&id, "run").await.unwrap();
        let events = store.events(&id, 0).await.unwrap();
        assert_eq!(
            events
                .iter()
                .find(|event| event.kind == "turn_files")
                .unwrap()
                .data["files"],
            json!([])
        );
    }
}
