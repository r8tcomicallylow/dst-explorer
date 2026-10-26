use std::collections::{HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use percent_encoding::percent_decode_str;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, RANGE};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};

use crate::api::{self, Resolution, USER_AGENT as UA};
use crate::models::Album;
use crate::{persist, store};

const QUEUE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    #[default]
    Queued,
    Paused,
    Resolving,
    Downloading,
    Extracting,
    Done,
    Failed,
    Cancelled,
    Manual,
    Retrying {
        attempt: usize,
        max_retries: usize,
    },
}

impl DownloadState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Paused => "Paused",
            Self::Resolving => "Finding link",
            Self::Downloading => "Downloading",
            Self::Extracting => "Extracting",
            Self::Done => "Complete",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Manual => "Needs browser",
            Self::Retrying { attempt, max_retries } => {
                if attempt == max_retries {
                    "Retrying (final attempt)"
                } else {
                    "Retrying…"
                }
            }
        }
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Resolving
                | Self::Downloading
                | Self::Extracting
                | Self::Retrying { .. }
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Cancelled | Self::Manual
        )
    }
}

#[derive(Default)]
pub struct Progress {
    downloaded: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn restore(&self, downloaded: u64, total: u64) {
        self.downloaded.store(downloaded, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }
}

pub struct DownloadTask {
    pub id: u64,
    pub album: Album,
    pub state: DownloadState,
    pub progress: Arc<Progress>,
    pub error: Option<String>,
    pub manual_url: Option<String>,
    pub output: Option<PathBuf>,
    pub extracted: Option<PathBuf>,
    pub warning: Option<String>,
    pub created: Instant,
}

impl DownloadTask {
    pub fn fraction(&self) -> Option<f32> {
        let total = self.progress.total();
        if total == 0 {
            None
        } else {
            Some((self.progress.downloaded() as f32 / total as f32).clamp(0.0, 1.0))
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedTask {
    id: u64,
    album: Album,
    state: DownloadState,
    downloaded: u64,
    total: u64,
    error: Option<String>,
    manual_url: Option<String>,
    output: Option<PathBuf>,
    extracted: Option<PathBuf>,
    warning: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct PersistedQueue {
    version: u32,
    next_id: u64,
    tasks: Vec<PersistedTask>,
}

impl Default for PersistedQueue {
    fn default() -> Self {
        Self {
            version: QUEUE_VERSION,
            next_id: 1,
            tasks: Vec::new(),
        }
    }
}

enum DlEvent {
    State {
        id: u64,
        state: DownloadState,
    },
    Completed {
        id: u64,
        path: PathBuf,
        extracted: Option<PathBuf>,
        warning: Option<String>,
    },
    Failed {
        id: u64,
        error: String,
    },
    Manual {
        id: u64,
        url: String,
        reason: String,
    },
    Cancelled {
        id: u64,
    },
}

#[derive(Clone, Debug)]
pub enum DownloadNotice {
    Completed { title: String, output: PathBuf },
    Failed { title: String, error: String },
}

pub struct DownloadManager {
    pub tasks: Vec<DownloadTask>,
    queue: VecDeque<u64>,
    tx: Sender<DlEvent>,
    rx: Receiver<DlEvent>,
    next_id: u64,
    active: usize,
    max_concurrent: usize,
    download_dir: PathBuf,
    auto_extract: bool,
    auto_retry: bool,
    max_retries: usize,
    /// Tasks whose worker is being stopped so they can be resumed later. The
    /// worker sends `Cancelled`; until then we must not let its state updates
    /// overwrite the paused state.
    pause_requested: HashSet<u64>,
    /// Number of unfinished tasks restored from disk on startup.
    pub restored_paused: usize,
}

impl DownloadManager {
    pub fn new(
        max_concurrent: usize,
        download_dir: PathBuf,
        auto_extract: bool,
        auto_retry: bool,
        max_retries: usize,
    ) -> Self {
        let (tx, rx) = unbounded();
        let mut manager = Self {
            tasks: Vec::new(),
            queue: VecDeque::new(),
            tx,
            rx,
            next_id: 1,
            active: 0,
            max_concurrent: max_concurrent.max(1),
            download_dir,
            auto_extract,
            auto_retry,
            max_retries,
            pause_requested: HashSet::new(),
            restored_paused: 0,
        };
        manager.restore();
        manager
    }

    fn restore(&mut self) {
        let stored = persist::read_json::<PersistedQueue>(&store::queue_path()).unwrap_or_default();
        if stored.tasks.is_empty() {
            return;
        }
        let mut next_id = stored.next_id.max(1);
        for mut saved in stored.tasks {
            next_id = next_id.max(saved.id.saturating_add(1));
            let progress = Arc::new(Progress::default());
            progress.restore(saved.downloaded, saved.total);
            let state = if saved.state.is_active() || saved.state == DownloadState::Paused {
                self.restored_paused += 1;
                DownloadState::Paused
            } else {
                saved.state
            };
            // Interrupted work is paused instead of starting automatically, so
            // a slow or unwanted download never begins behind the user's back.
            if state == DownloadState::Paused {
                saved.error = None;
            }
            self.tasks.push(DownloadTask {
                id: saved.id,
                album: saved.album,
                state,
                progress,
                error: saved.error,
                manual_url: saved.manual_url,
                output: saved.output,
                extracted: saved.extracted,
                warning: saved.warning,
                created: Instant::now(),
            });
        }
        self.next_id = next_id;
    }

    fn snapshot(&self) -> PersistedQueue {
        PersistedQueue {
            version: QUEUE_VERSION,
            next_id: self.next_id,
            tasks: self
                .tasks
                .iter()
                .map(|task| PersistedTask {
                    id: task.id,
                    album: task.album.clone(),
                    state: task.state,
                    downloaded: task.progress.downloaded(),
                    total: task.progress.total(),
                    error: task.error.clone(),
                    manual_url: task.manual_url.clone(),
                    output: task.output.clone(),
                    extracted: task.extracted.clone(),
                    warning: task.warning.clone(),
                })
                .collect(),
        }
    }

    /// Write the current queue to disk. Progress is snapshotted from the
    /// atomics, so this is safe to call while workers are running.
    pub fn persist(&self) {
        if let Err(err) = persist::write_json(&store::queue_path(), &self.snapshot()) {
            eprintln!("Could not save download queue: {err}");
        }
    }

    pub fn configure(
        &mut self,
        max_concurrent: usize,
        download_dir: PathBuf,
        auto_extract: bool,
        auto_retry: bool,
        max_retries: usize,
    ) {
        self.max_concurrent = max_concurrent.max(1);
        self.download_dir = download_dir;
        self.auto_extract = auto_extract;
        self.auto_retry = auto_retry;
        self.max_retries = max_retries;
        self.dispatch();
    }

    pub fn enqueue(&mut self, album: Album) -> Option<u64> {
        if let Some(existing) = self
            .tasks
            .iter()
            .find(|t| t.album.id == album.id && t.state.is_active())
        {
            return Some(existing.id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.tasks.push(DownloadTask {
            id,
            album: album.clone(),
            state: DownloadState::Queued,
            progress: Arc::new(Progress::default()),
            error: None,
            manual_url: None,
            output: None,
            extracted: None,
            warning: None,
            created: Instant::now(),
        });
        self.tasks.sort_by(|a, b| b.created.cmp(&a.created));
        self.queue.push_back(id);
        self.persist();
        self.dispatch();
        Some(id)
    }

    pub fn pause(&mut self, id: u64) {
        let mut changed = false;
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            match task.state {
                DownloadState::Queued => {
                    self.queue.retain(|x| *x != id);
                    task.state = DownloadState::Paused;
                    task.error = None;
                    changed = true;
                }
                DownloadState::Resolving
                | DownloadState::Downloading
                | DownloadState::Extracting => {
                    if self.pause_requested.insert(id) {
                        task.progress.cancel();
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            self.persist();
        }
    }

    pub fn resume(&mut self, id: u64) {
        let mut should_queue = false;
        self.pause_requested.remove(&id);
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            if task.state == DownloadState::Paused {
                task.state = DownloadState::Queued;
                task.progress = Arc::new(Progress::default());
                task.error = None;
                task.manual_url = None;
                task.warning = None;
                task.created = Instant::now();
                should_queue = true;
            }
        }
        if should_queue {
            self.queue.push_back(id);
            self.persist();
            self.dispatch();
        }
    }

    pub fn pause_all(&mut self) {
        let ids: Vec<u64> = self
            .tasks
            .iter()
            .filter(|t| t.state.is_active() || t.state == DownloadState::Paused)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.pause(id);
        }
    }

    pub fn resume_all(&mut self) {
        let ids: Vec<u64> = self
            .tasks
            .iter()
            .filter(|t| t.state == DownloadState::Paused)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.resume(id);
        }
    }

    pub fn retry_all_failed(&mut self) {
        let ids: Vec<u64> = self
            .tasks
            .iter()
            .filter(|t| t.state == DownloadState::Failed)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.retry(id);
        }
    }

    pub fn cancel(&mut self, id: u64) {
        let mut changed = false;
        self.pause_requested.remove(&id);
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            if task.state == DownloadState::Paused {
                task.state = DownloadState::Cancelled;
                task.error = Some("Cancelled.".into());
                changed = true;
            } else if task.state.is_active() {
                task.progress.cancel();
                if task.state == DownloadState::Queued {
                    self.queue.retain(|x| *x != id);
                    task.state = DownloadState::Cancelled;
                    task.error = Some("Cancelled before it started.".into());
                    changed = true;
                }
            }
        }
        if changed {
            self.persist();
        }
        self.dispatch();
    }

    pub fn retry(&mut self, id: u64) {
        let mut should_queue = false;
        self.pause_requested.remove(&id);
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            if task.state.is_terminal() || task.state == DownloadState::Paused {
                task.state = DownloadState::Queued;
                task.progress = Arc::new(Progress::default());
                task.error = None;
                task.manual_url = None;
                task.output = None;
                task.extracted = None;
                task.warning = None;
                task.created = Instant::now();
                should_queue = true;
            }
        }
        if should_queue {
            self.queue.push_back(id);
            self.persist();
            self.dispatch();
        }
    }

    pub fn remove(&mut self, id: u64) {
        self.pause_requested.remove(&id);
        if self
            .tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.state.is_active())
            .unwrap_or(false)
        {
            self.cancel(id);
            if let Some(task) = self.tasks.iter().find(|t| t.id == id) {
                if task.state.is_active() {
                    return;
                }
            }
        }
        self.queue.retain(|x| *x != id);
        self.tasks.retain(|t| t.id != id);
        self.persist();
    }

    pub fn clear_finished(&mut self) {
        self.tasks.retain(|t| !t.state.is_terminal());
        let queued: HashSet<u64> = self
            .tasks
            .iter()
            .filter(|t| t.state == DownloadState::Queued)
            .map(|t| t.id)
            .collect();
        self.queue.retain(|id| queued.contains(id));
        self.persist();
    }

    /// Forget completed tasks for an album after the user removes it from the
    /// library, so the library sync does not immediately restore it.
    pub fn forget_done_album(&mut self, album_id: u64) {
        let before = self.tasks.len();
        self.tasks
            .retain(|task| !(task.album.id == album_id && task.state == DownloadState::Done));
        if self.tasks.len() != before {
            self.persist();
        }
    }

    pub fn active_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state.is_active() || t.state == DownloadState::Paused)
            .count()
    }

    pub fn running_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.state.is_active()).count()
    }

    pub fn paused_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state == DownloadState::Paused)
            .count()
    }

    pub fn failed_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state == DownloadState::Failed)
            .count()
    }

    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    pub fn poll(&mut self) -> Vec<DownloadNotice> {
        let mut notices = Vec::new();
        let mut changed = false;
        loop {
            let event = match self.rx.try_recv() {
                Ok(event) => event,
                Err(_) => break,
            };
            match event {
                DlEvent::State { id, state } => {
                    if self.pause_requested.contains(&id) {
                        // Keep showing the task as active until the worker has
                        // acknowledged the stop request.
                        continue;
                    }
                    if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                        if !task.progress.is_cancelled() || state == DownloadState::Cancelled {
                            task.state = state;
                            changed = true;
                        }
                    }
                }
                DlEvent::Completed {
                    id,
                    path,
                    extracted,
                    warning,
                } => {
                    self.pause_requested.remove(&id);
                    if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                        task.state = DownloadState::Done;
                        task.output = Some(path.clone());
                        task.extracted = extracted;
                        task.warning = warning;
                        notices.push(DownloadNotice::Completed {
                            title: task.album.title.clone(),
                            output: path,
                        });
                    }
                    self.finish_one();
                    changed = true;
                }
                DlEvent::Failed { id, error } => {
                    self.pause_requested.remove(&id);
                    if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                        if task.progress.is_cancelled() {
                            task.state = DownloadState::Paused;
                        } else {
                            task.state = DownloadState::Failed;
                            task.error = Some(error.clone());
                            notices.push(DownloadNotice::Failed {
                                title: task.album.title.clone(),
                                error,
                            });
                        }
                    }
                    self.finish_one();
                    changed = true;
                }
                DlEvent::Manual { id, url, reason } => {
                    self.pause_requested.remove(&id);
                    if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                        if task.progress.is_cancelled() {
                            task.state = DownloadState::Paused;
                        } else {
                            task.state = DownloadState::Manual;
                            task.manual_url = Some(url);
                            task.error = Some(reason);
                        }
                    }
                    self.finish_one();
                    changed = true;
                }
                DlEvent::Cancelled { id } => {
                    let paused = self.pause_requested.remove(&id);
                    if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                        task.state = if paused {
                            DownloadState::Paused
                        } else {
                            DownloadState::Cancelled
                        };
                        if !paused {
                            task.error = Some("Cancelled.".into());
                        } else {
                            task.error = None;
                        }
                    }
                    self.finish_one();
                    changed = true;
                }
            }
        }
        self.dispatch();
        if changed {
            self.persist();
        }
        notices
    }

    fn finish_one(&mut self) {
        self.active = self.active.saturating_sub(1);
    }

    fn dispatch(&mut self) {
        let mut started = false;
        while self.active < self.max_concurrent {
            let id = match self.queue.pop_front() {
                Some(id) => id,
                None => break,
            };
            let (album, progress) = match self.tasks.iter_mut().find(|t| t.id == id) {
                Some(task) => {
                    if task.state != DownloadState::Queued {
                        continue;
                    }
                    task.state = DownloadState::Resolving;
                    task.created = Instant::now();
                    (task.album.clone(), task.progress.clone())
                }
                None => continue,
            };
            self.active += 1;
            started = true;
            let tx = self.tx.clone();
            let dir = self.download_dir.clone();
            let auto_extract = self.auto_extract;
            let auto_retry = self.auto_retry;
            let max_retries = self.max_retries;
            let _ = thread::Builder::new()
                .name(format!("download-{id}"))
                .spawn(move || run_download(id, album, progress, dir, auto_extract, auto_retry, max_retries, tx));
        }
        if started {
            self.persist();
        }
    }
}

fn run_download(
    id: u64,
    album: Album,
    progress: Arc<Progress>,
    download_dir: PathBuf,
    auto_extract: bool,
    auto_retry: bool,
    max_retries: usize,
    tx: Sender<DlEvent>,
) {
    let mut attempt = 0;
    let base_delay = Duration::from_secs(2);
    let max_delay = Duration::from_secs(30);

    loop {
        if progress.is_cancelled() {
            let _ = tx.send(DlEvent::Cancelled { id });
            return;
        }

        if attempt > 0 {
            let delay = base_delay
                .mul_f32(2.0_f32.powi(attempt as i32 - 1))
                .min(max_delay);
            let _ = tx.send(DlEvent::State {
                id,
                state: DownloadState::Retrying { attempt, max_retries },
            });
            std::thread::sleep(delay);
        }

        let _ = tx.send(DlEvent::State {
            id,
            state: if attempt == 0 {
                DownloadState::Resolving
            } else {
                DownloadState::Downloading
            },
        });

        let resolution = api::resolve_download(album.id);
        if progress.is_cancelled() {
            let _ = tx.send(DlEvent::Cancelled { id });
            return;
        }

        let direct = match resolution {
            Ok(Resolution::Direct(link)) => link,
            Ok(Resolution::Manual { url, reason }) => {
                let _ = tx.send(DlEvent::Manual { id, url, reason });
                return;
            }
            Err(error) => {
                if should_retry(&error, auto_retry, attempt, max_retries) {
                    attempt += 1;
                    continue;
                }
                let _ = tx.send(DlEvent::Failed { id, error });
                return;
            }
        };

        let _ = tx.send(DlEvent::State {
            id,
            state: DownloadState::Downloading,
        });

        let client = match Client::builder()
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(20))
            .redirect(Policy::limited(10))
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                let error = format!("Could not start HTTP client: {err}");
                if should_retry(&error, auto_retry, attempt, max_retries) {
                    attempt += 1;
                    continue;
                }
                let _ = tx.send(DlEvent::Failed { id, error });
                return;
            }
        };

        let downloaded = match download_file(
            &client,
            &direct.url,
            &direct.headers,
            &album,
            &download_dir,
            &progress,
        ) {
            Ok(path) => path,
            Err(DownloadError::Cancelled) => {
                let _ = tx.send(DlEvent::Cancelled { id });
                return;
            }
            Err(DownloadError::Failed(error)) => {
                if should_retry(&error, auto_retry, attempt, max_retries) {
                    attempt += 1;
                    continue;
                }
                let _ = tx.send(DlEvent::Failed { id, error });
                return;
            }
        };

        let mut extracted = None;
        let mut warning = None;
        if auto_extract && is_zip(&downloaded) {
            let _ = tx.send(DlEvent::State {
                id,
                state: DownloadState::Extracting,
            });
            match extract_zip(&downloaded) {
                Ok(folder) => extracted = Some(folder),
                Err(error) => warning = Some(format!("Downloaded, but could not extract: {error}")),
            }
        }

        let _ = tx.send(DlEvent::Completed {
            id,
            path: downloaded,
            extracted,
            warning,
        });
        return;
    }
}

fn should_retry(error: &str, auto_retry: bool, attempt: usize, max_retries: usize) -> bool {
    if !auto_retry || attempt >= max_retries {
        return false;
    }
    let lower = error.to_ascii_lowercase();
    // Retry on temporary network errors
    lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("temporary failure")
        || lower.contains("tls error")
        || lower.contains("ssl error")
        || lower.contains("dns")
        || lower.contains("name resolution")
        || lower.contains("http 5")
        || lower.contains("http 429")
        || lower.contains("http 503")
        || lower.contains("http 504")
        || lower.contains("http 502")
        || lower.contains("http 520")
        || lower.contains("http 521")
        || lower.contains("http 522")
        || lower.contains("http 523")
        || lower.contains("http 524")
}

enum DownloadError {
    Cancelled,
    Failed(String),
}

fn download_file(
    client: &Client,
    url: &str,
    headers: &[(String, String)],
    album: &Album,
    download_dir: &Path,
    progress: &Progress,
) -> Result<PathBuf, DownloadError> {
    if let Err(err) = fs::create_dir_all(download_dir) {
        return Err(DownloadError::Failed(format!(
            "Could not create download folder: {err}"
        )));
    }

    // Probe first (without Range) so the final filename from the response headers is known.
    // The .part file must be named after that final file, otherwise a retry can't find it.
    let probe_name = filename_hint(url, headers, album);
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }

    let mut response = request
        .send()
        .map_err(|e| DownloadError::Failed(format!("Download error: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(DownloadError::Failed(format!(
            "File host returned HTTP {status}"
        )));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let disposition = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if content_type.contains("text/html") && disposition.is_none() {
        return Err(DownloadError::Failed(
            "The host returned a web page instead of a file. Try opening it in your browser."
                .into(),
        ));
    }

    let mut final_name = disposition
        .as_deref()
        .and_then(filename_from_disposition)
        .unwrap_or(probe_name);
    if !has_extension(&final_name) {
        if let Some(ext) = extension_for_content_type(&content_type) {
            final_name.push_str(ext);
        }
    }
    let final_name = sanitize_filename(&final_name);
    let final_path = unique_path(download_dir, &final_name);
    let mut part_os = final_path.as_os_str().to_os_string();
    part_os.push(".part");
    let part_path = PathBuf::from(part_os);

    let existing = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    let mut append = false;

    // If a partial file from an earlier attempt exists, ask again with a Range request.
    // The probe response above is discarded in that case.
    if existing > 0 {
        drop(response);
        let mut resumed = client.get(url);
        for (name, value) in headers {
            resumed = resumed.header(name.as_str(), value.as_str());
        }
        response = resumed
            .header(RANGE, format!("bytes={existing}-"))
            .send()
            .map_err(|e| DownloadError::Failed(format!("Download error: {e}")))?;
        let resumed_status = response.status();
        if resumed_status.as_u16() == 206 {
            append = true;
        } else if resumed_status.is_success() {
            // The host ignored the range and sent the whole file again.
            let _ = fs::remove_file(&part_path);
        } else {
            return Err(DownloadError::Failed(format!(
                "File host returned HTTP {resumed_status} while resuming"
            )));
        }
    }

    let start_bytes = if append { existing } else { 0 };

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part_path)
        .map_err(|e| DownloadError::Failed(format!("Could not create file: {e}")))?;
    let mut writer = BufWriter::with_capacity(256 * 1024, file);

    let remaining = response.content_length().unwrap_or(0);
    let total = if append {
        existing.saturating_add(remaining)
    } else {
        remaining
    };
    progress.total.store(total, Ordering::Relaxed);
    progress.downloaded.store(start_bytes, Ordering::Relaxed);

    let mut buffer = vec![0u8; 256 * 1024];
    let mut downloaded = start_bytes;
    loop {
        if progress.is_cancelled() {
            let _ = writer.flush();
            return Err(DownloadError::Cancelled);
        }
        let read = match response.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) => {
                let _ = writer.flush();
                return Err(DownloadError::Failed(format!(
                    "Transfer interrupted: {err}"
                )));
            }
        };
        writer
            .write_all(&buffer[..read])
            .map_err(|e| DownloadError::Failed(format!("Could not write file: {e}")))?;
        downloaded += read as u64;
        progress.downloaded.store(downloaded, Ordering::Relaxed);
    }

    writer
        .flush()
        .map_err(|e| DownloadError::Failed(format!("Could not finish file: {e}")))?;
    drop(writer);

    if progress.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    if total > 0 && downloaded < total && progress.total() > 0 {
        // A truncated transfer is kept as .part so Retry can resume it.
        return Err(DownloadError::Failed(format!(
            "Connection closed early ({} of {}). Retry to resume.",
            crate::store::human_size(downloaded),
            crate::store::human_size(total)
        )));
    }

    fs::rename(&part_path, &final_path)
        .map_err(|e| DownloadError::Failed(format!("Could not finalize file: {e}")))?;
    Ok(final_path)
}

fn filename_hint(url: &str, headers: &[(String, String)], album: &Album) -> String {
    let _ = headers;
    if let Ok(parsed) = reqwest::Url::parse(url) {
        if let Some(segment) = parsed.path_segments().and_then(|s| s.last()) {
            let decoded = percent_decode_str(segment).decode_utf8_lossy();
            if !decoded.trim().is_empty() && decoded.contains('.') {
                return sanitize_filename(decoded.trim());
            }
        }
    }
    let mut name = album.title.clone();
    if name.trim().is_empty() {
        name = format!("album-{}", album.id);
    }
    if !has_extension(&name) {
        if !album.format.is_empty() {
            name.push_str(&format!(" [{}]", album.format));
        }
        name.push_str(".zip");
    }
    sanitize_filename(&name)
}

fn filename_from_disposition(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename*=") {
            let rest = rest.trim_matches('"');
            let encoded = rest
                .split("''")
                .nth(1)
                .or_else(|| rest.split('\'').nth(2))
                .unwrap_or(rest);
            let decoded = percent_decode_str(encoded).decode_utf8_lossy().to_string();
            if !decoded.trim().is_empty() {
                return Some(decoded);
            }
        }
        if let Some(rest) = part.strip_prefix("filename=") {
            let decoded = rest.trim_matches('"').replace("\\\"", "\"");
            if !decoded.trim().is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

fn extension_for_content_type(content_type: &str) -> Option<&'static str> {
    if content_type.contains("application/zip") {
        Some(".zip")
    } else if content_type.contains("application/x-rar") {
        Some(".rar")
    } else if content_type.contains("application/x-7z") {
        Some(".7z")
    } else if content_type.contains("application/gzip") {
        Some(".gz")
    } else if content_type.contains("audio/flac") {
        Some(".flac")
    } else if content_type.contains("audio/mpeg") {
        Some(".mp3")
    } else {
        None
    }
}

fn has_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .map(|ext| !ext.is_empty())
        .unwrap_or(false)
}

pub fn sanitize_filename(name: &str) -> String {
    let invalid = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let mut out: String = name
        .chars()
        .map(|c| {
            if invalid.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    out = out.trim().trim_matches('.').to_string();
    if out.chars().count() > 150 {
        if let Some(dot) = out.rfind('.') {
            let ext = out[dot..].to_string();
            let mut truncated = out[..dot].chars().take(140).collect::<String>();
            truncated.push_str(&ext);
            out = truncated;
        } else {
            out = out.chars().take(150).collect();
        }
    }
    if out.is_empty() {
        "download".into()
    } else {
        out
    }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for counter in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({counter}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}{ext}", crate::store::now_unix()))
}

fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}

fn extract_zip(zip_path: &Path) -> Result<PathBuf, String> {
    let file = File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("invalid ZIP: {e}"))?;
    let parent = zip_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = zip_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "album".into());
    let mut folder = parent.join(sanitize_filename(&stem));
    if folder.exists() {
        for counter in 2..10_000 {
            let candidate = parent.join(format!("{} ({counter})", sanitize_filename(&stem)));
            if !candidate.exists() {
                folder = candidate;
                break;
            }
        }
    }
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("could not read ZIP entry: {e}"))?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let out_path = folder.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = File::create(&out_path).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "hits the live DoujinStyle site and writes a temporary file"]
    fn live_download_and_resume() {
        let client = Client::builder()
            .user_agent(UA)
            .redirect(Policy::limited(10))
            .build()
            .expect("client");
        let dir = std::env::temp_dir().join(format!("doujin-test-{}", crate::store::now_unix()));
        fs::create_dir_all(&dir).expect("temp dir");
        let url = "https://doujinstyle.com/thumbs/32257.jpg";
        let album = Album {
            id: 32257,
            title: "Test album".into(),
            ..Album::default()
        };
        let progress = Progress::default();

        let do_download = || {
            download_file(&client, url, &[], &album, &dir, &progress).unwrap_or_else(|error| {
                match error {
                    DownloadError::Cancelled => panic!("download cancelled"),
                    DownloadError::Failed(message) => panic!("download failed: {message}"),
                }
            })
        };

        let path = do_download();
        let bytes = fs::read(&path).expect("read download");
        println!("downloaded {} bytes to {}", bytes.len(), path.display());
        assert!(bytes.len() > 1000, "file was suspiciously small");
        assert_eq!(bytes.len() as u64, progress.downloaded());
        assert_eq!(bytes.len() as u64, progress.total());

        // Leave a fake partial file with a zeroed prefix, then resume. If the host honours
        // Range requests the zeroed prefix survives, proving only the remainder was fetched.
        let part = dir.join("32257.jpg.part");
        fs::write(&part, vec![0u8; 1000]).expect("write partial");
        fs::remove_file(&path).expect("remove complete file");

        let resumed = do_download();
        let resumed_bytes = fs::read(&resumed).expect("read resumed");
        assert_eq!(
            resumed_bytes.len(),
            bytes.len(),
            "resumed file size differs"
        );
        if resumed_bytes[..1000].iter().all(|b| *b == 0) {
            println!("resume verified: server returned a 206 and the prefix was kept");
        } else {
            println!("note: host ignored Range, file was downloaded again from scratch");
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
