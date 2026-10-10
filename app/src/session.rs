//! Everything the views share: loaded databases, the frame store, the decode
//! cache, the data source (log file or live bus) and recording.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use canbus::{Bitrate, Frame, FrameStore, SeriesCache, candump};
use dbc::{CompiledDb, Dbc, ParseError, SignalRef};

/// One loaded (and possibly edited) database.
pub struct DbFile {
    pub path: Option<PathBuf>,
    pub dbc: Dbc,
    pub dirty: bool,
    pub errors: Vec<ParseError>,
    /// Excluded from decoding without unloading.
    pub enabled: bool,
}

impl DbFile {
    pub fn name(&self) -> String {
        self.path.as_deref().map_or_else(|| "untitled.dbc".into(), crate::util::file_name)
    }
}

pub enum Source {
    None,
    Log(PathBuf),
    #[cfg(target_os = "linux")]
    Live(canbus::socket::LiveBus),
}

struct Loading {
    path: PathBuf,
    total: u64,
    progress: Arc<AtomicU64>,
    rx: Receiver<std::io::Result<candump::Loaded>>,
    started: Instant,
}

struct Recorder {
    path: PathBuf,
    writer: candump::Writer<std::fs::File>,
    frames: u64,
}

/// A notice for the status bar.
pub struct Notice {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct Session {
    pub dbs: Vec<DbFile>,
    pub db: CompiledDb,
    /// Bumped whenever `db` is recompiled so views can re-resolve signal paths.
    pub db_gen: u64,
    pub store: FrameStore,
    /// Bumped whenever `store` is replaced (not on append).
    pub store_gen: u64,
    pub cache: SeriesCache,
    pub source: Source,
    pub bitrate: Bitrate,
    /// Live capture keeps running but views stop following new data.
    pub paused: bool,
    loading: Option<Loading>,
    recorder: Option<Recorder>,
    pub notice: Option<Notice>,
    scratch: Vec<Frame>,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            dbs: Vec::new(),
            db: CompiledDb::default(),
            db_gen: 0,
            store: FrameStore::default(),
            store_gen: 0,
            cache: SeriesCache::default(),
            source: Source::None,
            bitrate: Bitrate::default(),
            paused: false,
            loading: None,
            recorder: None,
            notice: None,
            scratch: Vec::new(),
        }
    }
}

impl Session {
    pub fn info(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::info!("{text}");
        self.notice = Some(Notice { text, error: false, at: Instant::now() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::error!("{text}");
        self.notice = Some(Notice { text, error: true, at: Instant::now() });
    }

    // --- databases ------------------------------------------------------------

    pub fn recompile(&mut self) {
        self.db = CompiledDb::new(self.dbs.iter().filter(|d| d.enabled).map(|d| &d.dbc));
        self.db_gen += 1;
        self.cache.clear();
    }

    pub fn open_dbc(&mut self, path: &Path) {
        match dbc::read_file(path) {
            Ok((dbc, errors)) => {
                let msg = format!(
                    "Loaded {}: {} messages, {} signals{}",
                    crate::util::file_name(path),
                    dbc.messages.len(),
                    dbc.signal_count(),
                    if errors.is_empty() { String::new() } else { format!(", {} problems", errors.len()) }
                );
                let file = DbFile { path: Some(path.to_path_buf()), dbc, dirty: false, errors, enabled: true };
                match self.dbs.iter().position(|d| d.path.as_deref() == Some(path)) {
                    Some(i) => self.dbs[i] = file,
                    None => self.dbs.push(file),
                }
                self.recompile();
                if !self.db.conflicts.is_empty() {
                    let ids: Vec<String> = self.db.conflicts.iter().map(|k| canbus::frame::fmt_key(*k)).collect();
                    self.error(format!("{msg} — duplicate ids ignored: {}", ids.join(", ")));
                } else {
                    self.info(msg);
                }
            }
            Err(e) => self.error(format!("Cannot read {}: {e}", path.display())),
        }
    }

    pub fn new_dbc(&mut self) -> usize {
        self.dbs.push(DbFile {
            path: None,
            dbc: Dbc { nodes: vec![dbc::Node::new("ECU")], ..Default::default() },
            dirty: true,
            errors: Vec::new(),
            enabled: true,
        });
        self.recompile();
        self.dbs.len() - 1
    }

    pub fn save_dbc(&mut self, idx: usize, save_as: bool) {
        let db = &self.dbs[idx];
        let path = match (&db.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => crate::util::dialog::save(&db.name(), "dbc"),
        };
        let Some(path) = path else { return };
        match std::fs::write(&path, dbc::write(&self.dbs[idx].dbc)) {
            Ok(()) => {
                let db = &mut self.dbs[idx];
                db.path = Some(path.clone());
                db.dirty = false;
                self.info(format!("Saved {}", path.display()));
            }
            Err(e) => self.error(format!("Save failed: {e}")),
        }
    }

    pub fn close_dbc(&mut self, idx: usize) {
        self.dbs.remove(idx);
        self.recompile();
    }

    /// Decoded series for a signal (lazily decoded, extended on append).
    pub fn series(&mut self, r: SignalRef) -> &canbus::Series {
        self.cache.get(&self.store, &self.db, r)
    }

    // --- data sources -----------------------------------------------------------

    fn replace_store(&mut self, store: FrameStore) {
        self.store = store;
        self.store_gen += 1;
        self.cache.clear();
    }

    pub fn open_log(&mut self, path: &Path) {
        self.disconnect();
        let total = std::fs::metadata(path).map_or(1, |m| m.len().max(1));
        let progress = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        {
            let (path, progress) = (path.to_path_buf(), progress.clone());
            std::thread::spawn(move || {
                let _ = tx.send(candump::load(&path, &progress));
            });
        }
        self.loading = Some(Loading { path: path.to_path_buf(), total, progress, rx, started: Instant::now() });
    }

    /// Loading progress in 0..=1, if a log is being loaded.
    pub fn loading_progress(&self) -> Option<(String, f32)> {
        let l = self.loading.as_ref()?;
        Some((crate::util::file_name(&l.path), l.progress.load(Ordering::Relaxed) as f32 / l.total as f32))
    }

    #[cfg(target_os = "linux")]
    pub fn connect(&mut self, iface: &str, ctx: &eframe::egui::Context) {
        self.disconnect();
        let ctx = ctx.clone();
        let wake = move || ctx.request_repaint_after(std::time::Duration::from_millis(30));
        match canbus::socket::LiveBus::open(iface, wake) {
            Ok(bus) => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64().floor());
                let mut store = FrameStore::with_t0(now);
                store.channel_index(iface);
                self.replace_store(store);
                self.source = Source::Live(bus);
                self.paused = false;
                self.info(format!("Connected to {iface}"));
            }
            Err(e) => self.error(format!("Cannot open {iface}: {e}")),
        }
    }

    pub fn disconnect(&mut self) {
        self.stop_recording();
        #[cfg(target_os = "linux")]
        if let Source::Live(bus) = &self.source {
            let name = bus.iface.clone();
            self.source = Source::None;
            self.info(format!("Disconnected from {name}"));
        }
    }

    pub fn is_live(&self) -> bool {
        #[cfg(target_os = "linux")]
        if matches!(self.source, Source::Live(_)) {
            return true;
        }
        false
    }

    #[cfg(target_os = "linux")]
    pub fn live(&self) -> Option<&canbus::socket::LiveBus> {
        match &self.source {
            Source::Live(b) => Some(b),
            _ => None,
        }
    }

    pub fn source_label(&self) -> String {
        match &self.source {
            Source::None => "no data".into(),
            Source::Log(p) => crate::util::file_name(p),
            #[cfg(target_os = "linux")]
            Source::Live(b) => format!("live: {}", b.iface),
        }
    }

    /// Clear captured data (live) or unload the log.
    pub fn clear_data(&mut self) {
        let t0 = if self.is_live() { now() } else { 0.0 };
        let mut store = FrameStore::with_t0(t0);
        store.channels = self.store.channels.clone();
        self.replace_store(store);
        if !self.is_live() {
            self.source = Source::None;
        }
    }

    /// Per-frame housekeeping: finish background loads, drain the live bus.
    pub fn poll(&mut self, ctx: &eframe::egui::Context) {
        if let Some(l) = &self.loading {
            match l.rx.try_recv() {
                Ok(res) => {
                    let l = self.loading.take().unwrap();
                    match res {
                        Ok(loaded) => {
                            let msg = format!(
                                "Loaded {}: {} frames, {} ids in {:.2?}{}",
                                crate::util::file_name(&l.path),
                                loaded.store.len(),
                                loaded.store.keys.len(),
                                l.started.elapsed(),
                                if loaded.bad_lines > 0 { format!(" ({} unparsable lines)", loaded.bad_lines) } else { String::new() }
                            );
                            self.replace_store(loaded.store);
                            self.source = Source::Log(l.path);
                            self.info(msg);
                        }
                        Err(e) => self.error(format!("Cannot load {}: {e}", l.path.display())),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
                Err(mpsc::TryRecvError::Disconnected) => self.loading = None,
            }
        }

        #[cfg(target_os = "linux")]
        if let Source::Live(bus) = &self.source {
            let mut frames = std::mem::take(&mut self.scratch);
            let alive = bus.drain(&mut frames);
            let error = (!alive).then(|| bus.error()).flatten();
            let iface = bus.iface.clone();
            if !frames.is_empty() {
                let chan = self.store.channel_index(&iface);
                for f in &mut frames {
                    f.channel = chan;
                    self.store.push(f);
                }
                if let Some(rec) = &mut self.recorder {
                    for f in &frames {
                        if let Err(e) = rec.writer.write(f, &iface) {
                            log::error!("recording: {e}");
                        }
                    }
                    rec.frames += frames.len() as u64;
                }
            }
            frames.clear();
            self.scratch = frames;
            if !alive {
                self.source = Source::None;
                self.stop_recording();
                self.error(format!("{iface}: {}", error.unwrap_or_else(|| "receive thread stopped".into())));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    /// Live time (relative), or the end of the loaded data.
    pub fn now_rel(&self) -> f64 {
        if self.is_live() && !self.paused { now() - self.store.t0 } else { self.store.time_range().map_or(0.0, |r| r.1) }
    }

    // --- recording / export -----------------------------------------------------

    pub fn start_recording(&mut self) {
        let name = chrono::Local::now().format("candump-%Y-%m-%d_%H%M%S.log").to_string();
        let Some(path) = crate::util::dialog::save(&name, "log") else { return };
        match candump::Writer::create(&path) {
            Ok(writer) => {
                self.info(format!("Recording to {}", path.display()));
                self.recorder = Some(Recorder { path, writer, frames: 0 });
            }
            Err(e) => self.error(format!("Cannot record: {e}")),
        }
    }

    pub fn stop_recording(&mut self) {
        if let Some(mut rec) = self.recorder.take() {
            let _ = rec.writer.flush();
            self.info(format!("Recorded {} frames to {}", rec.frames, rec.path.display()));
        }
    }

    pub fn recording(&self) -> Option<(String, u64)> {
        self.recorder.as_ref().map(|r| (crate::util::file_name(&r.path), r.frames))
    }

    /// Save frames in a relative time window as a candump log.
    pub fn save_range(&mut self, t0: f64, t1: f64) {
        let range = self.store.range(t0, t1);
        let n = range.len();
        let Some(path) = crate::util::dialog::save("trimmed.log", "log") else { return };
        match candump::save(&self.store, range, &path) {
            Ok(()) => self.info(format!("Saved {n} frames to {}", path.display())),
            Err(e) => self.error(format!("Save failed: {e}")),
        }
    }
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}
