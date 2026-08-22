//! Server-side stem split: reuse the TUI cache, run the same recipe on
//! a miss, and stream the resulting FLACs to LAN clients.
//!
//! Phones cannot run demucs / audio-separator. The server never
//! auto-provisions an engine — that needs the TUI's consent overlay.
//! A cache hit from a previous TUI `M`-press is served immediately.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use zytunes::cache::Logger;
use zytunes::stems::provision::find_engine;
use zytunes::stems::{
    cached_stems, default_stem_cache_dir, migrate_legacy_stem_entries, recipe_separator,
    stem_cache_key, store_stems, RecipeKind, StemError, StemKind, STEM_EXT,
};

#[derive(Debug, Clone)]
pub enum EngineLookup {
    /// `[stems] command` then PATH then the managed install.
    Discover,
    /// Tests: never spawn, always report the engine-missing error.
    Missing,
}

#[derive(Debug, Clone)]
pub struct StemSettings {
    pub recipe: RecipeKind,
    pub demucs_model: String,
    pub cache_max_bytes: u64,
    pub command: Option<PathBuf>,
    pub cache_dir: PathBuf,
    pub engine: EngineLookup,
}

impl StemSettings {
    /// Default recipe/cache plus optional `[stems]` overlay from the
    /// same config.toml the TUI reads. `None` when `$HOME` cannot be
    /// resolved (no place to put the cache).
    pub fn load() -> Option<Self> {
        let cache_dir = default_stem_cache_dir()?;
        let mut settings = Self {
            recipe: RecipeKind::Demucs,
            demucs_model: "htdemucs_6s".into(),
            cache_max_bytes: 10u64.saturating_mul(1 << 30),
            command: None,
            cache_dir,
            engine: EngineLookup::Discover,
        };
        settings.apply_toml();
        Some(settings)
    }

    fn apply_toml(&mut self) {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let path = Path::new(&home)
            .join(".config")
            .join("zytunes")
            .join("config.toml");
        let Ok(contents) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(table) = contents.parse::<toml::Table>() else {
            return;
        };
        let Some(stems) = table.get("stems").and_then(|v| v.as_table()) else {
            return;
        };
        if let Some(raw) = stems.get("recipe").and_then(|v| v.as_str()) {
            match raw.trim() {
                "" => {}
                other => match other.parse::<RecipeKind>() {
                    Ok(kind) => self.recipe = kind,
                    Err(_) => eprintln!(
                        "zytunes-serve: unknown [stems] recipe {other:?}, using {}",
                        self.recipe
                    ),
                },
            }
        }
        if let Some(model) = stems.get("model").and_then(|v| v.as_str()) {
            if !model.is_empty() {
                self.demucs_model = model.to_string();
            }
        }
        if let Some(gb) = stems.get("cache_max_gb").and_then(|v| v.as_integer()) {
            if gb > 0 {
                self.cache_max_bytes = (gb as u64).saturating_mul(1 << 30);
            }
        }
        if let Some(cmd) = stems.get("command").and_then(|v| v.as_str()) {
            if !cmd.is_empty() {
                self.command = Some(PathBuf::from(cmd));
            }
        }
        // Same blank-as-unset rule as `StemsConfig::stem_cache_dir`.
        if let Some(dir) = stems.get("cache_dir").and_then(|v| v.as_str()) {
            let trimmed = dir.trim();
            if !trimmed.is_empty() {
                self.cache_dir = PathBuf::from(trimmed);
            }
        }
    }

    fn cache_id(&self) -> String {
        self.recipe.cache_id(&self.demucs_model)
    }
}

#[derive(Clone)]
pub struct StemHub {
    settings: Arc<StemSettings>,
    inner: Arc<Mutex<HubInner>>,
}

struct HubInner {
    job: Option<ActiveJob>,
}

struct ActiveJob {
    track_id: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU8>,
    error: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StemJobStatus {
    Ready,
    Missing,
    Separating,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct StemFileDto {
    pub kind: String,
    pub label: String,
    pub short_label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StemSetDto {
    pub status: StemJobStatus,
    pub recipe: String,
    pub layout: Vec<String>,
    pub stems: Vec<StemFileDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub engine_available: bool,
}

impl StemSetDto {
    pub fn unavailable() -> Self {
        Self {
            status: StemJobStatus::Failed,
            recipe: RecipeKind::Demucs.to_string(),
            layout: Vec::new(),
            stems: Vec::new(),
            progress: None,
            error: Some("stem cache unavailable".into()),
            engine_available: false,
        }
    }
}

fn log() -> Logger {
    Arc::new(|msg: &str| eprintln!("[stems] {msg}"))
}

fn stem_dtos(id: u64, layout: &[StemKind]) -> (Vec<String>, Vec<StemFileDto>) {
    let names: Vec<String> = layout.iter().map(|k| k.file_stem().to_string()).collect();
    let stems = layout
        .iter()
        .map(|k| StemFileDto {
            kind: k.file_stem().to_string(),
            label: k.label().to_string(),
            short_label: k.short_label().to_string(),
            url: format!("/tracks/{id}/stems/{}", k.file_stem()),
        })
        .collect();
    (names, stems)
}

impl StemHub {
    pub fn new(settings: StemSettings) -> Self {
        Self {
            settings: Arc::new(settings),
            inner: Arc::new(Mutex::new(HubInner { job: None })),
        }
    }

    pub fn from_env() -> Option<Self> {
        StemSettings::load().map(Self::new)
    }

    fn resolve_engine(&self) -> Option<PathBuf> {
        match &self.settings.engine {
            EngineLookup::Missing => None,
            EngineLookup::Discover => find_engine(
                self.settings.recipe.engine(),
                self.settings.command.as_deref(),
            ),
        }
    }

    fn engine_available(&self) -> bool {
        self.resolve_engine().is_some()
    }

    fn dto(
        &self,
        id: u64,
        status: StemJobStatus,
        progress: Option<u8>,
        error: Option<String>,
    ) -> StemSetDto {
        let layout = self.settings.recipe.layout();
        let (names, stems) = stem_dtos(id, layout);
        StemSetDto {
            status,
            recipe: self.settings.recipe.to_string(),
            layout: names,
            stems,
            progress,
            error,
            engine_available: self.engine_available(),
        }
    }

    fn cached(&self, source: &Path) -> bool {
        let log = log();
        migrate_legacy_stem_entries(&self.settings.cache_dir, &log);
        cached_stems(
            &self.settings.cache_dir,
            source,
            &self.settings.cache_id(),
            self.settings.recipe.layout(),
            &log,
        )
        .is_some()
    }

    pub fn status(&self, id: u64, source: &Path) -> StemSetDto {
        if self.cached(source) {
            return self.dto(id, StemJobStatus::Ready, None, None);
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = inner.job.as_ref() {
            if job.track_id == id {
                if let Some(error) = job.error.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    return self.dto(id, StemJobStatus::Failed, None, Some(error));
                }
                let progress = job.progress.load(Ordering::Relaxed);
                return self.dto(id, StemJobStatus::Separating, Some(progress), None);
            }
        }
        self.dto(id, StemJobStatus::Missing, None, None)
    }

    pub fn start(&self, id: u64, source: PathBuf) -> StemSetDto {
        if self.cached(&source) {
            return self.dto(id, StemJobStatus::Ready, None, None);
        }
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(job) = inner.job.as_ref() {
                if job.track_id == id
                    && job
                        .error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_none()
                    && !job.cancel.load(Ordering::SeqCst)
                {
                    let progress = job.progress.load(Ordering::Relaxed);
                    return self.dto(id, StemJobStatus::Separating, Some(progress), None);
                }
            }
        }
        let Some(command) = self.resolve_engine() else {
            return self.dto(
                id,
                StemJobStatus::Failed,
                None,
                Some("stem engine not installed — press M in zytunes-tui once to provision".into()),
            );
        };
        let separator =
            match recipe_separator(self.settings.recipe, &self.settings.demucs_model, command) {
                Ok(s) => s,
                Err(error) => return self.dto(id, StemJobStatus::Failed, None, Some(error)),
            };

        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU8::new(0));
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(prev) = inner.job.as_ref() {
                prev.cancel.store(true, Ordering::SeqCst);
            }
            inner.job = Some(ActiveJob {
                track_id: id,
                cancel: Arc::clone(&cancel),
                progress: Arc::clone(&progress),
                error: Arc::clone(&error),
            });
        }

        let settings = Arc::clone(&self.settings);
        let inner = Arc::clone(&self.inner);
        thread::spawn(move || {
            let log = log();
            migrate_legacy_stem_entries(&settings.cache_dir, &log);
            let cache_id = settings.cache_id();
            let layout = settings.recipe.layout();
            if cached_stems(&settings.cache_dir, &source, &cache_id, layout, &log).is_some() {
                clear_job_if(&inner, &cancel);
                return;
            }
            let work_dir = settings
                .cache_dir
                .join("work")
                .join(stem_cache_key(&source.to_string_lossy(), &cache_id));
            let _ = std::fs::remove_dir_all(&work_dir);
            let on_progress = {
                let progress = Arc::clone(&progress);
                move |pct: u8| progress.store(pct, Ordering::Relaxed)
            };
            let on_line = |line: &str| log(line);
            let cancelled = || cancel.load(Ordering::SeqCst);
            let outcome =
                separator.separate(&source, &work_dir, &cancelled, &on_progress, &on_line);
            match outcome {
                Ok(produced) => {
                    if let Err(e) = store_stems(
                        &settings.cache_dir,
                        &source,
                        &cache_id,
                        &produced,
                        settings.cache_max_bytes,
                        &log,
                    ) {
                        *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    }
                }
                Err(StemError::Cancelled) => {}
                Err(e) => {
                    *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                }
            }
            let _ = std::fs::remove_dir_all(&work_dir);
            clear_job_if(&inner, &cancel);
        });

        self.dto(id, StemJobStatus::Separating, Some(0), None)
    }

    pub fn cancel(&self, id: u64, source: &Path) -> StemSetDto {
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(job) = inner.job.as_ref() {
                if job.track_id == id {
                    job.cancel.store(true, Ordering::SeqCst);
                }
            }
        }
        self.status(id, source)
    }

    pub fn stem_path(&self, source: &Path, kind: StemKind) -> Option<PathBuf> {
        if !self.cached(source) {
            return None;
        }
        let layout = self.settings.recipe.layout();
        if !layout.contains(&kind) {
            return None;
        }
        let entry = self.settings.cache_dir.join(stem_cache_key(
            &source.to_string_lossy(),
            &self.settings.cache_id(),
        ));
        let path = entry.join(format!("{}.{STEM_EXT}", kind.file_stem()));
        path.is_file().then_some(path)
    }
}

fn clear_job_if(inner: &Mutex<HubInner>, cancel: &Arc<AtomicBool>) {
    let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
    let Some(job) = guard.job.as_ref() else {
        return;
    };
    // A successor job has a different token; leaving it alone is the
    // whole point of supersede.
    if !Arc::ptr_eq(&job.cancel, cancel) {
        return;
    }
    let failed = job
        .error
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    if !failed {
        guard.job = None;
    }
}
