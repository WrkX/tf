use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tf_audio::{Meter, Recorder};
use tf_llm::{LocalConfig, Refiner, Request as LlmRequest};
use tf_meeting::{Capture, CaptureOptions, Segmenter, Speaker, summary};
use tf_store::{
    DiarizationState, DiarizedSegment, DiarizedTranscript, MeetingId, MeetingStatus, NewMeeting, NewSegment,
    NewTake, Outcome as StoreOutcome, SpeakerTarget, Store,
};
use tf_stt::{EngineSpec, TranscribeOptions, Transcriber};

use crate::config::Config;
use crate::mode::{Backend, Mode, ModeId, RAW_ID};
use crate::prompt;

pub const RAW_MODE: &str = RAW_ID;

const LEVEL_INTERVAL: Duration = Duration::from_millis(16);
const IDLE_POLL: Duration = Duration::from_millis(100);
const PRUNE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const MIN_AUDIO_MS: u64 = 500;
const DEFAULT_TIMEOUT_MS: u32 = 8000;
const MEETING_DRAIN_INTERVAL: Duration = Duration::from_millis(100);
const MEETING_RESAMPLE_MS: u64 = 500;
const BLEED_MAX_SEGMENT_MS: u64 = 2_500;

#[derive(Clone, Debug)]
pub struct MeetingAudio {
    pub meeting_id: MeetingId,
    pub speaker: Speaker,
    pub start_ms: u64,
    pub end_ms: u64,
    pub samples: Vec<f32>,
}

/// jobs werden von einem einzigen langlebigen worker in reihenfolge verarbeitet.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Job {
    Start {
        app: Option<String>,
        app_name: Option<String>,
        /// gesetzt, wenn ein hotkey des modus den take gestartet hat.
        mode: Option<ModeId>,
    },
    Stop,
    Cancel,
    SetMode(ModeId),
    Reload(Config),
    /// lädt die aktuelle engine neu, auch wenn sich ihre id nicht geändert hat. genutzt nach einem modell-download, damit der ane-compile sofort passiert.
    Warm,
    InsertDone(StoreOutcome),
    Rerun {
        take_id: i64,
        engine: Option<String>,
    },
    DeleteTakes(Vec<i64>),
    DeleteAllAudio,
    DeleteAllHistory,
    StartMeeting {
        app: Option<String>,
        app_name: Option<String>,
        pids: Vec<i32>,
    },
    StopMeeting,
    RegenerateSummary(MeetingId),
    DeleteMeetings(Vec<MeetingId>),
    MeetingSummaryReady {
        meeting_id: MeetingId,
        result: Result<String, String>,
        mode: String,
    },
    DiarizationProgress {
        meeting_id: MeetingId,
        fraction: f32,
    },
    DiarizationReady {
        meeting_id: MeetingId,
        result: Result<tf_diarize::Diarization, String>,
        path: PathBuf,
        offset_ms: u64,
        keep_audio: bool,
    },
    RerunDiarization(MeetingId),
    SpeakerEdit(SpeakerEdit),
}

/// korrekturen vom nutzer, die keinen diarizer brauchen.
#[derive(Clone, Debug)]
pub enum SpeakerEdit {
    Rename { speaker_id: i64, name: Option<String> },
    Assign { segment_ids: Vec<i64>, speaker_id: i64 },
    NewSpeaker { segment_ids: Vec<i64> },
    Merge { from: i64, into: i64 },
    Split { speaker_id: i64, groups: Vec<Vec<i64>> },
}

#[derive(Clone, Debug)]
pub struct Take {
    pub raw_text: String,
    pub final_text: String,
    pub duration_ms: u64,
    pub stt_ms: u64,
    pub llm_ms: Option<u64>,
    pub engine: String,
    pub mode: ModeId,
    pub mode_name: String,
    /// `backend/modell`, wenn der llm-schritt lief.
    pub llm: Option<String>,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Failure {
    ModelMissing(PathBuf),
    Audio(String),
    Engine(String),
    /// der llm-schritt ist gescheitert; stattdessen wurde das rohe transkript genutzt.
    Refine(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Self::ModelMissing(path) => format!(
                "Speech model not installed ({})",
                path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            ),
            Self::Audio(e) => format!("Could not record audio: {e}"),
            Self::Engine(e) => format!("Transcription failed: {e}"),
            Self::Refine(e) => format!("Used the raw transcript: {e}"),
        }
    }

    pub fn is_model_missing(&self) -> bool {
        matches!(self, Self::ModelMissing(_))
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    EngineLoading,
    EngineReady,
    EngineFailed(Failure),
    Recording {
        mode: ModeId,
    },
    ModeChanged {
        mode: ModeId,
    },
    /// geglätteter eingangs-pegel, `0.0..=1.0`.
    Level(f32),
    Processing,
    Ready(Take),
    Cancelled {
        empty: bool,
    },
    Failed(Failure),
    /// einmalige, dem nutzer zeigbare meldung (z. b. ein fehlendes angeheftetes mikrofon).
    Notice(String),
    /// zeilen wurden hinzugefügt, gelöscht oder gekürzt; das history-fenster sollte neu laden.
    HistoryChanged,
    RerunDone(i64),
    MeetingStarted {
        id: MeetingId,
        title: String,
        app_name: Option<String>,
        app_bundle_id: Option<String>,
    },
    MeetingSegment {
        meeting_id: MeetingId,
        speaker: Speaker,
        start_ms: u64,
        end_ms: u64,
        text: String,
    },
    MeetingFinished {
        id: MeetingId,
    },
    MeetingSummaryReady {
        id: MeetingId,
        summary: String,
    },
    MeetingFailed(String),
    DiarizationStarted {
        id: MeetingId,
    },
    DiarizationProgress {
        id: MeetingId,
        fraction: f32,
    },
    DiarizationFinished {
        id: MeetingId,
    },
    DiarizationFailed {
        id: MeetingId,
        message: String,
    },
}

/// zugriff auf den pipeline-worker. ihn zu verwerfen stoppt den worker nicht.
pub struct Core {
    jobs: Sender<Job>,
}

impl Core {
    pub fn spawn(config: Config, models_root: PathBuf, on_event: impl Fn(Event) + Send + 'static) -> Self {
        let (jobs, rx) = mpsc::channel();
        let job_tx = jobs.clone();
        let spawned = std::thread::Builder::new()
            .name("tf-pipeline".into())
            .spawn(move || worker(config, models_root, rx, job_tx, on_event));
        if let Err(e) = spawned {
            crate::log_error!("could not start pipeline worker: {e}");
        }
        Self { jobs }
    }

    pub fn start(&self, app: Option<String>, app_name: Option<String>, mode: Option<ModeId>) {
        self.send(Job::Start { app, app_name, mode });
    }

    pub fn set_mode(&self, mode: ModeId) {
        self.send(Job::SetMode(mode));
    }

    pub fn stop(&self) {
        self.send(Job::Stop);
    }

    pub fn cancel(&self) {
        self.send(Job::Cancel);
    }

    pub fn reload(&self, config: Config) {
        self.send(Job::Reload(config));
    }

    pub fn warm(&self) {
        self.send(Job::Warm);
    }

    pub fn insert_done(&self, outcome: StoreOutcome) {
        self.send(Job::InsertDone(outcome));
    }

    pub fn rerun(&self, take_id: i64, engine: Option<String>) {
        self.send(Job::Rerun { take_id, engine });
    }

    pub fn delete_takes(&self, ids: Vec<i64>) {
        self.send(Job::DeleteTakes(ids));
    }

    pub fn delete_all_history(&self) {
        self.send(Job::DeleteAllHistory);
    }

    pub fn delete_all_audio(&self) {
        self.send(Job::DeleteAllAudio);
    }

    pub fn start_meeting(&self, app: Option<String>, app_name: Option<String>, pids: Vec<i32>) {
        self.send(Job::StartMeeting { app, app_name, pids });
    }

    pub fn stop_meeting(&self) {
        self.send(Job::StopMeeting);
    }

    pub fn regenerate_summary(&self, meeting_id: MeetingId) {
        self.send(Job::RegenerateSummary(meeting_id));
    }

    pub fn delete_meetings(&self, ids: Vec<MeetingId>) {
        self.send(Job::DeleteMeetings(ids));
    }

    pub fn rerun_diarization(&self, meeting_id: MeetingId) {
        self.send(Job::RerunDiarization(meeting_id));
    }

    pub fn speaker_edit(&self, edit: SpeakerEdit) {
        self.send(Job::SpeakerEdit(edit));
    }

    fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            crate::log_error!("pipeline worker is gone");
        }
    }
}

struct Active {
    recorder: Recorder,
    meter: Meter,
    started: Instant,
    app: Option<String>,
    app_name: Option<String>,
    mode: Mode,
}

struct Pending {
    take: Take,
    created_at: i64,
    /// nur behalten, wenn audio-speicherung aktiv ist.
    samples: Option<Vec<f32>>,
}

struct Loaded {
    spec: EngineSpec,
    engine: Box<dyn Transcriber>,
}

enum MeetingMsg {
    Audio(MeetingAudio),
    Done { id: MeetingId, ended_at: i64, them_offset_ms: u64 },
    Failed { id: MeetingId, message: String },
}

struct ActiveMeeting {
    id: MeetingId,
    app_bundle_id: Option<String>,
    app_name: Option<String>,
    stop: Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct DiarizationRequest {
    meeting_id: MeetingId,
    path: PathBuf,
    offset_ms: u64,
    keep_audio: bool,
}

struct TurnQueue {
    meeting_id: MeetingId,
    path: PathBuf,
    samples: Vec<f32>,
    offset_ms: u64,
    keep_audio: bool,
    turns: Vec<tf_diarize::Turn>,
    centroids: Vec<Vec<f32>>,
    next: usize,
    segments: Vec<DiarizedSegment>,
}

enum EngineMsg {
    Ready { spec: EngineSpec, engine: Box<dyn Transcriber> },
    Failed { spec: EngineSpec, failure: Failure },
}

/// hält höchstens ein parakeet-modell geladen und lädt neu, wenn sich die standard-engine ändert.
struct Engines {
    spec: EngineSpec,
    loaded: Option<Loaded>,
    loading: Option<EngineSpec>,
    failure: Option<Failure>,
    last_used: Instant,
}

impl Engines {
    fn new(spec: EngineSpec) -> Self {
        Self { spec, loaded: None, loading: None, failure: None, last_used: Instant::now() }
    }

    /// gibt das geladene modell nach `idle_secs` ohne nutzung frei; 0 behält es.
    fn unload_if_idle(&mut self, idle_secs: u32) {
        if idle_secs > 0
            && self.loaded.is_some()
            && self.last_used.elapsed() >= Duration::from_secs(u64::from(idle_secs))
        {
            self.loaded = None;
        }
    }

    /// startet das laden im hintergrund, wenn eine aufnahme auf einem nicht geladenen modell beginnt.
    fn prewarm(&mut self, load_tx: &Sender<EngineSpec>, emit: &impl Fn(Event)) {
        self.last_used = Instant::now();
        if !self.is_loaded() && self.loading.is_none() && self.failure.is_none() {
            self.start_load(load_tx, emit);
        }
    }

    fn set_spec(&mut self, spec: EngineSpec, load_tx: &Sender<EngineSpec>, emit: &impl Fn(Event)) {
        if self.spec == spec {
            return;
        }
        self.spec = spec;
        self.loaded = None;
        self.failure = None;
        self.loading = None;
        self.start_load(load_tx, emit);
    }

    fn warm(&mut self, load_tx: &Sender<EngineSpec>, emit: &impl Fn(Event)) {
        self.loaded = None;
        self.failure = None;
        self.loading = None;
        self.start_load(load_tx, emit);
    }

    fn start_load(&mut self, load_tx: &Sender<EngineSpec>, emit: &impl Fn(Event)) {
        if self.loading.is_some() {
            return;
        }
        let spec = self.spec.clone();
        self.loading = Some(spec.clone());
        emit(Event::EngineLoading);
        if load_tx.send(spec).is_err() {
            self.loading = None;
            let failure = Failure::Engine("engine loader stopped".into());
            self.failure = Some(failure.clone());
            emit(Event::EngineFailed(failure));
        }
    }

    fn ensure(
        &mut self,
        rx: &Receiver<EngineMsg>,
        load_tx: &Sender<EngineSpec>,
        emit: &impl Fn(Event),
    ) -> Result<(), Failure> {
        self.last_used = Instant::now();
        if self.is_loaded() {
            return Ok(());
        }
        if self.loading.is_none()
            && let Some(failure) = self.failure.clone()
        {
            return Err(failure);
        }
        if self.loading.is_none() {
            self.start_load(load_tx, emit);
        }
        while !self.is_loaded() {
            match rx.recv() {
                Ok(message) => self.handle(message, emit),
                Err(_) => return Err(Failure::Engine("engine worker stopped".into())),
            }
            if self.loading.is_none()
                && let Some(failure) = self.failure.clone()
            {
                return Err(failure);
            }
        }
        Ok(())
    }

    fn drain(&mut self, rx: &Receiver<EngineMsg>, emit: &impl Fn(Event)) {
        while let Ok(message) = rx.try_recv() {
            self.handle(message, emit);
        }
    }

    fn handle(&mut self, message: EngineMsg, emit: &impl Fn(Event)) {
        match message {
            EngineMsg::Ready { spec, engine } => {
                if spec == self.spec {
                    self.last_used = Instant::now();
                    self.loaded = Some(Loaded { spec, engine });
                    self.loading = None;
                    self.failure = None;
                    emit(Event::EngineReady);
                }
            }
            EngineMsg::Failed { spec, failure } => {
                if spec == self.spec {
                    self.loading = None;
                    self.failure = Some(failure.clone());
                    emit(Event::EngineFailed(failure));
                }
            }
        }
    }

    fn is_loaded(&self) -> bool {
        self.loaded.as_ref().map(|loaded| loaded.spec == self.spec).unwrap_or(false)
    }
}

enum WarmMsg {
    Ready(String, Box<dyn Refiner>),
    Failed(String, String),
}

/// hält einen refiner pro `(backend, modell)` im cache und startet lokale server in einem hintergrund-thread, damit der worker beim laden pegel weiter schickt.
struct Refiners {
    cache: BTreeMap<String, Box<dyn Refiner>>,
    warming: BTreeMap<String, Instant>,
    tx: Sender<WarmMsg>,
    rx: Receiver<WarmMsg>,
}

impl Refiners {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { cache: BTreeMap::new(), warming: BTreeMap::new(), tx, rx }
    }

    fn clear(&mut self) {
        self.cache.clear();
        self.warming.clear();
    }

    fn key(mode: &Mode) -> Option<String> {
        mode.refine().map(|refine| refine.backend.cache_key(refine.model.as_deref()))
    }

    fn warm_async(&mut self, mode: &Mode, config: &Config, models_root: &Path) {
        let Some(key) = Self::key(mode) else {
            return;
        };
        if self.cache.contains_key(&key) || self.warming.contains_key(&key) {
            return;
        }
        let spec = match refiner_spec(mode, config, models_root) {
            Ok(spec) => spec,
            Err(e) => {
                crate::log_warn!("cannot prepare refiner {key}: {e}");
                return;
            }
        };
        let tx = self.tx.clone();
        let cleanup = key.clone();
        let warm_key = key.clone();
        self.warming.insert(key, Instant::now());
        let spawned =
            std::thread::Builder::new().name("tf-llm-warm".into()).spawn(move || match tf_llm::build(spec) {
                Ok(mut refiner) => {
                    refiner.warm();
                    let _ = tx.send(WarmMsg::Ready(warm_key, refiner));
                }
                Err(e) => {
                    let _ = tx.send(WarmMsg::Failed(warm_key, e.to_string()));
                }
            });
        if spawned.is_err() {
            self.warming.remove(&cleanup);
        }
    }

    fn drain(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                WarmMsg::Ready(key, refiner) => {
                    self.warming.remove(&key);
                    self.cache.insert(key, refiner);
                }
                WarmMsg::Failed(key, error) => {
                    self.warming.remove(&key);
                    crate::log_warn!("warm {key}: {error}");
                }
            }
        }
    }

    fn take(&mut self, mode: &Mode, config: &Config, models_root: &Path) -> Result<Box<dyn Refiner>, String> {
        let key = Self::key(mode).ok_or_else(|| "mode has no refinement step".to_owned())?;
        if let Some(refiner) = self.cache.remove(&key) {
            return Ok(refiner);
        }
        if self.warming.contains_key(&key) {
            let deadline = Instant::now() + Duration::from_secs(65);
            while Instant::now() < deadline {
                match self.rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(WarmMsg::Ready(warmed, refiner)) => {
                        self.warming.remove(&warmed);
                        if warmed == key {
                            return Ok(refiner);
                        }
                        self.cache.insert(warmed, refiner);
                    }
                    Ok(WarmMsg::Failed(warmed, error)) => {
                        self.warming.remove(&warmed);
                        if warmed == key {
                            return Err(error);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            return Err("the language model did not start".to_owned());
        }
        let spec = refiner_spec(mode, config, models_root)?;
        let mut refiner = tf_llm::build(spec).map_err(|e| e.to_string())?;
        refiner.warm();
        Ok(refiner)
    }

    fn put(&mut self, mode: &Mode, refiner: Box<dyn Refiner>) {
        if let Some(key) = Self::key(mode) {
            self.cache.insert(key, refiner);
        }
    }

    fn tick(&mut self) {
        for refiner in self.cache.values_mut() {
            refiner.tick();
        }
    }
}

/// baut die backend-spec für einen modus. public, damit der test-knopf in den einstellungen exakt dieselbe config nutzt wie die pipeline.
pub fn refiner_spec(mode: &Mode, config: &Config, models_root: &Path) -> Result<tf_llm::Spec, String> {
    let refine = mode.refine().ok_or_else(|| "mode has no refinement step".to_owned())?;
    match &refine.backend {
        Backend::Apple => Ok(tf_llm::Spec::Apple),
        Backend::Local => {
            let model = match refine.model.as_deref() {
                Some(id) => find_gguf(&models_root.join(id))
                    .ok_or_else(|| format!("the local model {id} is not installed"))?,
                None => first_installed_gguf(models_root)
                    .ok_or_else(|| "the mode has no local model".to_owned())?,
            };
            let runtime = find_runtime(models_root)
                .ok_or_else(|| "the llama.cpp runtime is not installed".to_owned())?;
            Ok(tf_llm::Spec::Local {
                config: LocalConfig {
                    runtime_dir: runtime,
                    log: crate::paths::llama_log_path(),
                    state_dir: crate::paths::support_dir(),
                    context: config.llm.local.context,
                    keep_alive: Duration::from_secs(u64::from(config.llm.local.keep_alive_minutes) * 60),
                },
                model,
            })
        }
        Backend::Endpoint(id) => {
            let endpoint = config
                .llm
                .endpoints
                .iter()
                .find(|endpoint| &endpoint.id == id)
                .ok_or_else(|| format!("unknown endpoint {id}"))?;
            Ok(tf_llm::Spec::Endpoint(tf_llm::Endpoint {
                base_url: endpoint.base_url.clone(),
                model: refine.model.clone().unwrap_or_default(),
                api_key: None,
            }))
        }
        Backend::Cloud(provider) => Err(format!("the cloud provider {provider} is not available yet")),
    }
}

/// der editor zeigt das erste installierte modell für einen modus ohne gespeichertes modell, also löst die pipeline es genauso auf.
fn first_installed_gguf(models_root: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        fs::read_dir(models_root).ok()?.filter_map(|entry| Some(entry.ok()?.path())).collect();
    dirs.sort();
    dirs.iter().find_map(|dir| find_gguf(dir))
}

fn find_gguf(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gguf")) {
                return Some(path);
            }
        }
    }
    None
}

fn find_runtime(models_root: &Path) -> Option<PathBuf> {
    let support = models_root.parent()?;
    let root = support.join("runtimes").join("llama.cpp");
    let entries = fs::read_dir(root).ok()?;
    entries.flatten().map(|entry| entry.path()).find(|dir| dir.is_dir())
}

fn worker(
    mut config: Config,
    models_root: PathBuf,
    jobs: Receiver<Job>,
    job_tx: Sender<Job>,
    emit: impl Fn(Event),
) {
    let (engine_tx, engine_rx) = mpsc::channel();
    let (load_tx, load_rx) = mpsc::channel();
    if let Err(e) =
        std::thread::Builder::new().name("tf-engine".into()).spawn(move || engine_loader(load_rx, engine_tx))
    {
        crate::log_error!("could not start the engine loader: {e}");
    }
    let mut engines = Engines::new(EngineSpec::parse(&config.engine.default, &models_root));
    engines.start_load(&load_tx, &emit);
    let mut refiners = Refiners::new();
    let (meeting_tx, meeting_rx) = mpsc::channel::<MeetingMsg>();
    let mut active_meeting: Option<ActiveMeeting> = None;
    let mut diarization_queue: VecDeque<DiarizationRequest> = VecDeque::new();
    let mut diarization_running: Option<MeetingId> = None;
    let mut diarization_cancel = Arc::new(AtomicBool::new(false));
    let mut turn_queue: Option<TurnQueue> = None;

    let mut history = open_history(&config);
    if let Some(store) = history.as_ref() {
        if let Ok(count) = store.mark_interrupted()
            && count > 0
        {
            crate::log_warn!("marked {count} interrupted meeting(s)");
        }
        prune_history(store, &config, &emit);
        sweep_audio(store);
    }
    let mut next_prune = Instant::now() + PRUNE_INTERVAL;
    let mut pending: Option<Pending> = None;
    let mut active: Option<Active> = None;
    let mut device_notified = false;

    loop {
        if active.is_some() {
            match jobs.recv_timeout(LEVEL_INTERVAL) {
                Ok(Job::Stop) => {
                    if let Some(a) = active.take()
                        && let Some(done) = finish(
                            a,
                            &mut engines,
                            &engine_rx,
                            &load_tx,
                            &config,
                            &models_root,
                            &mut refiners,
                            &emit,
                        )
                    {
                        pending = Some(done);
                    }
                }
                Ok(Job::Cancel) => {
                    if let Some(a) = active.take() {
                        let _ = a.recorder.stop();
                        emit(Event::Cancelled { empty: false });
                    }
                }
                Ok(Job::SetMode(id)) => {
                    if let Some(a) = active.as_mut() {
                        a.mode = config.mode(&id).unwrap_or_else(|| config.default_mode());
                        refiners.warm_async(&a.mode, &config, &models_root);
                        emit(Event::ModeChanged { mode: a.mode.id.clone() });
                    }
                }
                Ok(Job::Reload(c)) => {
                    refiners.clear();
                    apply_config(c, &mut config, &models_root, &mut engines, &mut history, &load_tx, &emit)
                }
                Ok(Job::Warm) => engines.warm(&load_tx, &emit),
                Ok(Job::StopMeeting) => {
                    if let Some(meeting) = active_meeting.as_ref() {
                        let _ = meeting.stop.send(());
                    }
                }
                Ok(Job::DiarizationProgress { meeting_id, fraction }) => {
                    emit(Event::DiarizationProgress { id: meeting_id, fraction });
                }
                Ok(Job::DiarizationReady { meeting_id, result, path, offset_ms, keep_audio }) => {
                    accept_diarization_ready(
                        meeting_id,
                        result,
                        path,
                        offset_ms,
                        keep_audio,
                        history.as_ref(),
                        &config,
                        &models_root,
                        &job_tx,
                        &emit,
                        &mut turn_queue,
                        &mut diarization_queue,
                        &mut diarization_running,
                        &mut diarization_cancel,
                    );
                }
                Ok(Job::SpeakerEdit(edit)) => {
                    if let Some(store) = history.as_ref() {
                        apply_speaker_edit(store, edit, &emit);
                    }
                }
                Ok(Job::RerunDiarization(meeting_id)) => {
                    if let Some(store) = history.as_ref() {
                        rerun_diarization(
                            store,
                            meeting_id,
                            &config,
                            &models_root,
                            &job_tx,
                            &emit,
                            &turn_queue,
                            &mut diarization_queue,
                            &mut diarization_running,
                            &mut diarization_cancel,
                        );
                    }
                }
                Ok(
                    Job::Start { .. }
                    | Job::InsertDone(_)
                    | Job::Rerun { .. }
                    | Job::DeleteTakes(_)
                    | Job::DeleteAllAudio
                    | Job::DeleteAllHistory
                    | Job::StartMeeting { .. }
                    | Job::RegenerateSummary(_)
                    | Job::DeleteMeetings(_)
                    | Job::MeetingSummaryReady { .. },
                ) => {}
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(a) = active.as_mut() {
                        let level = a.meter.update(a.recorder.level());
                        emit(Event::Level(level));
                    }
                    let full = active
                        .as_ref()
                        .map(|a| a.started.elapsed().as_secs() >= u64::from(config.audio.max_recording_secs))
                        .unwrap_or(false);
                    if full
                        && let Some(a) = active.take()
                        && let Some(done) = finish(
                            a,
                            &mut engines,
                            &engine_rx,
                            &load_tx,
                            &config,
                            &models_root,
                            &mut refiners,
                            &emit,
                        )
                    {
                        pending = Some(done);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
            refiners.drain();
            refiners.tick();
            engines.drain(&engine_rx, &emit);
            engines.unload_if_idle(config.engine.unload_after_secs);
        } else {
            match jobs.recv_timeout(IDLE_POLL) {
                Ok(Job::Start { app, app_name, mode }) => {
                    start(
                        &mut active,
                        app,
                        app_name,
                        mode.as_deref(),
                        &config,
                        &mut device_notified,
                        &mut refiners,
                        &models_root,
                        &emit,
                    );
                    engines.prewarm(&load_tx, &emit);
                }
                Ok(Job::Reload(c)) => {
                    refiners.clear();
                    apply_config(c, &mut config, &models_root, &mut engines, &mut history, &load_tx, &emit)
                }
                Ok(Job::Warm) => engines.warm(&load_tx, &emit),
                Ok(Job::InsertDone(outcome)) => {
                    if let Some(done) = pending.take() {
                        write_history(done, outcome, history.as_ref(), &config, &emit);
                    }
                }
                Ok(Job::Rerun { take_id, engine }) => {
                    if !config.history.store.stores_text() {
                        emit(Event::Notice("Turn on history saving to re-run a take.".into()));
                    } else if let Some(store) = history.as_ref() {
                        rerun(
                            store,
                            take_id,
                            engine,
                            &models_root,
                            &mut engines,
                            &engine_rx,
                            &load_tx,
                            &config,
                            &emit,
                        );
                    }
                }
                Ok(Job::DeleteTakes(ids)) => {
                    if let Some(store) = history.as_ref() {
                        delete_takes(store, &ids, &emit);
                    } else if let Some(store) = temporary_store() {
                        delete_takes(&store, &ids, &emit);
                    }
                }
                Ok(Job::DeleteAllAudio) => {
                    if let Some(store) = history.as_ref() {
                        delete_all_audio(store, &emit);
                    } else if let Some(store) = temporary_store() {
                        delete_all_audio(&store, &emit);
                    } else {
                        let _ = fs::remove_dir_all(crate::paths::audio_dir());
                        emit(Event::HistoryChanged);
                    }
                }
                Ok(Job::DeleteAllHistory) => {
                    if let Some(store) = history.as_ref() {
                        delete_all_history(store, &emit);
                    } else if let Some(store) = temporary_store() {
                        delete_all_history(&store, &emit);
                    } else {
                        let _ = fs::remove_dir_all(crate::paths::audio_dir());
                        emit(Event::HistoryChanged);
                    }
                }
                Ok(Job::StartMeeting { app, app_name, pids }) => {
                    if let Some(store) = history.as_ref() {
                        match start_meeting(store, app, app_name, pids, &config, &meeting_tx, &emit) {
                            Ok(meeting) => {
                                emit(Event::MeetingStarted {
                                    id: meeting.id,
                                    title: store
                                        .meeting(meeting.id)
                                        .ok()
                                        .flatten()
                                        .map(|meeting| meeting.title)
                                        .unwrap_or_else(|| "Meeting".into()),
                                    app_name: meeting.app_name.clone(),
                                    app_bundle_id: meeting.app_bundle_id.clone(),
                                });
                                active_meeting = Some(meeting);
                            }
                            Err(message) => emit(Event::MeetingFailed(message)),
                        }
                    } else {
                        emit(Event::MeetingFailed("The history database is unavailable.".into()));
                    }
                }
                Ok(Job::StopMeeting) => {
                    if let Some(meeting) = active_meeting.as_ref() {
                        let _ = meeting.stop.send(());
                    }
                }
                Ok(Job::RegenerateSummary(meeting_id)) => {
                    if let Some(store) = history.as_ref() {
                        spawn_summary(store, meeting_id, &config, &models_root, &job_tx, &emit);
                    }
                }
                Ok(Job::DiarizationProgress { meeting_id, fraction }) => {
                    emit(Event::DiarizationProgress { id: meeting_id, fraction });
                }
                Ok(Job::DiarizationReady { meeting_id, result, path, offset_ms, keep_audio }) => {
                    accept_diarization_ready(
                        meeting_id,
                        result,
                        path,
                        offset_ms,
                        keep_audio,
                        history.as_ref(),
                        &config,
                        &models_root,
                        &job_tx,
                        &emit,
                        &mut turn_queue,
                        &mut diarization_queue,
                        &mut diarization_running,
                        &mut diarization_cancel,
                    );
                }
                Ok(Job::RerunDiarization(meeting_id)) => {
                    if let Some(store) = history.as_ref() {
                        rerun_diarization(
                            store,
                            meeting_id,
                            &config,
                            &models_root,
                            &job_tx,
                            &emit,
                            &turn_queue,
                            &mut diarization_queue,
                            &mut diarization_running,
                            &mut diarization_cancel,
                        );
                    }
                }
                Ok(Job::SpeakerEdit(edit)) => {
                    if let Some(store) = history.as_ref() {
                        apply_speaker_edit(store, edit, &emit);
                    }
                }
                Ok(Job::DeleteMeetings(ids)) => {
                    let ids = ids
                        .into_iter()
                        .filter(|id| active_meeting.as_ref().map(|meeting| meeting.id) != Some(*id))
                        .collect::<Vec<_>>();
                    if ids.is_empty() {
                        emit(Event::Notice("Stop the meeting before deleting it.".into()));
                    } else {
                        if diarization_running.is_some_and(|id| ids.contains(&id)) {
                            diarization_cancel.store(true, Ordering::Relaxed);
                        }
                        diarization_queue.retain(|request| !ids.contains(&request.meeting_id));
                        if turn_queue.as_ref().is_some_and(|queue| ids.contains(&queue.meeting_id)) {
                            turn_queue = None;
                        }
                        if let Some(store) = history.as_ref() {
                            delete_meetings(store, &ids, &emit);
                        } else if let Some(store) = temporary_store() {
                            delete_meetings(&store, &ids, &emit);
                        }
                        if turn_queue.is_none() {
                            maybe_start_diarization(
                                &mut diarization_queue,
                                &mut diarization_running,
                                &mut diarization_cancel,
                                &config,
                                &models_root,
                                &job_tx,
                            );
                        }
                    }
                }
                Ok(Job::MeetingSummaryReady { meeting_id, result, mode }) => {
                    if let Some(store) = history.as_ref() {
                        match result {
                            Ok(summary) => match store.set_meeting_summary(meeting_id, &summary, &mode) {
                                Ok(()) => {
                                    emit(Event::MeetingSummaryReady { id: meeting_id, summary });
                                    emit(Event::HistoryChanged);
                                }
                                Err(error) => {
                                    crate::log_error!("could not save the meeting summary: {error}")
                                }
                            },
                            Err(error) => {
                                let _ = store.set_meeting_status(meeting_id, MeetingStatus::Done);
                                emit(Event::Notice(format!("The meeting summary failed: {error}")));
                                emit(Event::HistoryChanged);
                            }
                        }
                    }
                }
                Ok(Job::Stop | Job::Cancel | Job::SetMode(_)) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(message) = meeting_rx.try_recv() {
                match message {
                    MeetingMsg::Audio(audio) => {
                        if let Some(store) = history.as_ref() {
                            transcribe_meeting_audio(
                                store,
                                audio,
                                &mut engines,
                                &engine_rx,
                                &load_tx,
                                &config,
                                &emit,
                            );
                        }
                    }
                    MeetingMsg::Done { id, ended_at, them_offset_ms } => {
                        if let Some(mut meeting) = active_meeting.take() {
                            if let Some(thread) = meeting.thread.take() {
                                let _ = thread.join();
                            }
                            if let Some(store) = history.as_ref() {
                                if let Err(error) = store.finish_meeting(id, ended_at) {
                                    crate::log_error!("could not finish meeting {id}: {error}");
                                }
                                if let Err(error) = store.set_them_offset(id, them_offset_ms) {
                                    crate::log_error!("could not record the them timebase: {error}");
                                }
                                emit(Event::MeetingFinished { id });
                                emit(Event::HistoryChanged);
                                let models = diarization_models(&models_root);
                                let wants_diarization = config.meetings.identify_speakers
                                    && models.join("Segmentation.mlmodelc").exists();
                                if wants_diarization {
                                    let keep_audio = store
                                        .meeting(id)
                                        .ok()
                                        .flatten()
                                        .map(|meeting| meeting.audio_them.is_some())
                                        .unwrap_or(false);
                                    let _ = store.set_diarization(id, Some(DiarizationState::Running));
                                    emit(Event::DiarizationStarted { id });
                                    diarization_queue.push_back(DiarizationRequest {
                                        meeting_id: id,
                                        path: crate::paths::meeting_dir(id).join("them.m4a"),
                                        offset_ms: them_offset_ms,
                                        keep_audio,
                                    });
                                    if turn_queue.is_none() {
                                        maybe_start_diarization(
                                            &mut diarization_queue,
                                            &mut diarization_running,
                                            &mut diarization_cancel,
                                            &config,
                                            &models_root,
                                            &job_tx,
                                        );
                                    }
                                } else {
                                    if store
                                        .meeting(id)
                                        .ok()
                                        .flatten()
                                        .is_some_and(|meeting| meeting.audio_them.is_none())
                                    {
                                        let _ =
                                            fs::remove_file(crate::paths::meeting_dir(id).join("them.m4a"));
                                    }
                                    spawn_summary(store, id, &config, &models_root, &job_tx, &emit);
                                }
                            }
                        }
                    }
                    MeetingMsg::Failed { id, message } => {
                        if let Some(mut meeting) = active_meeting.take() {
                            if let Some(thread) = meeting.thread.take() {
                                let _ = thread.join();
                            }
                            if let Some(store) = history.as_ref() {
                                let _ = store.set_meeting_status(id, MeetingStatus::Interrupted);
                                if store
                                    .meeting(id)
                                    .ok()
                                    .flatten()
                                    .is_some_and(|meeting| meeting.audio_them.is_none())
                                {
                                    let _ = fs::remove_file(crate::paths::meeting_dir(id).join("them.m4a"));
                                }
                            }
                        }
                        emit(Event::MeetingFailed(message));
                    }
                }
            }
            if active.is_none() && turn_queue.is_some() {
                let mut queue = turn_queue.take().expect("checked above");
                if queue.next < queue.turns.len() {
                    let turn = queue.turns[queue.next].clone();
                    queue.next += 1;
                    let begin = (turn.start_ms as usize * 16_000 / 1_000).min(queue.samples.len());
                    let end = (turn.end_ms as usize * 16_000 / 1_000).min(queue.samples.len());
                    if end > begin {
                        match transcribe_once(
                            &mut engines,
                            &engine_rx,
                            &load_tx,
                            &queue.samples[begin..end],
                            &emit,
                        ) {
                            Ok((text, _)) => {
                                let text = config.vocabulary.apply(&text);
                                let text = text.trim();
                                if !text.is_empty() {
                                    queue.segments.push(DiarizedSegment {
                                        start_ms: turn.start_ms + queue.offset_ms,
                                        end_ms: turn.end_ms + queue.offset_ms,
                                        text: text.to_owned(),
                                        speaker: turn.speaker,
                                        overlap: turn.overlap,
                                        embedding: turn.embedding.clone(),
                                    });
                                }
                            }
                            Err(error) => crate::log_warn!("turn transcription failed: {}", error.message()),
                        }
                    }
                }
                if queue.next < queue.turns.len() {
                    turn_queue = Some(queue);
                } else if let Some(store) = history.as_ref() {
                    finish_turn_queue(store, queue, &config, &models_root, &job_tx, &emit);
                    maybe_start_diarization(
                        &mut diarization_queue,
                        &mut diarization_running,
                        &mut diarization_cancel,
                        &config,
                        &models_root,
                        &job_tx,
                    );
                }
            }
            if Instant::now() >= next_prune {
                next_prune = Instant::now() + PRUNE_INTERVAL;
                if let Some(store) = history.as_ref() {
                    prune_history(store, &config, &emit);
                }
            }
            refiners.drain();
            refiners.tick();
            engines.drain(&engine_rx, &emit);
            engines.unload_if_idle(config.engine.unload_after_secs);
        }
    }
}

/// ein langlebiger loader, damit nie zwei parakeet-modelle zugleich im speicher liegen. ein schwall an anfragen fällt auf die neueste zusammen.
fn engine_loader(loads: Receiver<EngineSpec>, tx: Sender<EngineMsg>) {
    while let Ok(mut spec) = loads.recv() {
        while let Ok(next) = loads.try_recv() {
            spec = next;
        }
        load_engine(spec, tx.clone());
    }
}

fn apply_config(
    next: Config,
    config: &mut Config,
    models_root: &Path,
    engines: &mut Engines,
    history: &mut Option<Store>,
    load_tx: &Sender<EngineSpec>,
    emit: &impl Fn(Event),
) {
    let _history_enabled = next.history.store.stores_text();
    *config = next;
    if history.is_none() {
        *history = open_history(config);
        if let Some(store) = history.as_ref() {
            sweep_audio(store);
        }
    }
    engines.set_spec(EngineSpec::parse(&config.engine.default, models_root), load_tx, emit);
}

#[allow(clippy::too_many_arguments)]
fn start(
    active: &mut Option<Active>,
    app: Option<String>,
    app_name: Option<String>,
    trigger_mode: Option<&str>,
    config: &Config,
    device_notified: &mut bool,
    refiners: &mut Refiners,
    models_root: &Path,
    emit: &impl Fn(Event),
) {
    if active.is_some() {
        return;
    }
    let mode = config.resolve_mode(trigger_mode, app.as_deref());
    match Recorder::start(config.audio.device.as_deref()) {
        Ok((recorder, fallback)) => {
            if let Some(uid) = fallback
                && !*device_notified
            {
                *device_notified = true;
                emit(Event::Notice(format!(
                    "The selected microphone ({uid}) is not connected. Using the system default."
                )));
            }
            *active = Some(Active {
                recorder,
                meter: Meter::default(),
                started: Instant::now(),
                app,
                app_name,
                mode: mode.clone(),
            });
            emit(Event::Recording { mode: mode.id.clone() });
            // das llm laden, während der nutzer spricht; der warm-thread blockiert weder aufnahme noch pegelanzeige.
            refiners.warm_async(&mode, config, models_root);
        }
        Err(e) => {
            crate::log_error!("could not open the microphone: {e}");
            emit(Event::Failed(Failure::Audio(e.to_string())));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish(
    active: Active,
    engines: &mut Engines,
    engine_rx: &Receiver<EngineMsg>,
    load_tx: &Sender<EngineSpec>,
    config: &Config,
    models_root: &Path,
    refiners: &mut Refiners,
    emit: &impl Fn(Event),
) -> Option<Pending> {
    let mode = active.mode.clone();
    let recording = active.recorder.stop();
    emit(Event::Processing);
    let total_ms = if recording.sample_rate == 0 {
        0
    } else {
        recording.samples.len() as u64 * 1000 / u64::from(recording.sample_rate)
    };
    if total_ms < 200 {
        emit(Event::Cancelled { empty: true });
        return None;
    }

    let samples = tf_audio::resample::to_16k(&recording.samples, recording.sample_rate);
    let mut trimmed = if config.audio.silence_removal {
        tf_audio::vad::trim(&samples, tf_stt::SAMPLE_RATE, config.audio.vad)
    } else {
        let speech_ms = (samples.len() as u64 * 1000 / u64::from(tf_stt::SAMPLE_RATE)) as u32;
        tf_audio::vad::Trimmed { samples, speech_ms }
    };
    if trimmed.samples.is_empty() {
        emit(Event::Cancelled { empty: true });
        return None;
    }

    if config.audio.normalize {
        tf_audio::normalize(&mut trimmed.samples);
    }

    let default_spec = EngineSpec::parse(&config.engine.default, models_root);
    let spec = mode
        .engine
        .as_deref()
        .map(|id| EngineSpec::parse(id, models_root))
        .unwrap_or_else(|| default_spec.clone());
    let switched = spec != engines.spec;
    if switched {
        engines.set_spec(spec, load_tx, emit);
    }
    let result = transcribe_mode(engines, engine_rx, load_tx, &trimmed.samples, &mode, config, emit);
    if switched {
        engines.set_spec(default_spec, load_tx, emit);
    }

    let (text, stt_ms, engine) = match result {
        Ok(value) => value,
        Err(failure) => {
            emit(Event::Failed(failure));
            return None;
        }
    };
    let raw = text.trim().to_owned();
    let pre = if mode.use_vocabulary {
        config.vocabulary.apply(&raw)
    } else {
        crate::text::apply_replacements(&raw, &config.vocabulary.replacements)
    };

    let (final_text, llm, llm_ms, refine_failure) = refine(&mode, &pre, config, models_root, refiners);
    crate::log_info!(
        "transcribed {total_ms} ms audio in {stt_ms} ms with {engine} ({}x realtime, mode {})",
        total_ms.checked_div(stt_ms).unwrap_or(0),
        mode.id
    );

    let take = Take {
        raw_text: raw,
        final_text,
        duration_ms: total_ms,
        stt_ms,
        llm_ms,
        engine,
        mode: mode.id.clone(),
        mode_name: mode.name.clone(),
        llm,
        app_bundle_id: active.app,
        app_name: active.app_name,
    };
    emit(Event::Ready(take.clone()));
    if let Some(reason) = refine_failure {
        emit(Event::Failed(Failure::Refine(reason)));
    }
    if !config.history.store.stores_text() {
        return None;
    }
    let samples = if config.history.store.stores_audio() && total_ms >= MIN_AUDIO_MS {
        Some(trimmed.samples)
    } else {
        None
    };
    Some(Pending { take, created_at: now_ms(), samples })
}

fn transcribe_mode(
    engines: &mut Engines,
    engine_rx: &Receiver<EngineMsg>,
    load_tx: &Sender<EngineSpec>,
    samples: &[f32],
    mode: &Mode,
    config: &Config,
    emit: &impl Fn(Event),
) -> Result<(String, u64, String), Failure> {
    engines.ensure(engine_rx, load_tx, emit)?;
    let Some(loaded) = engines.loaded.as_mut() else {
        return Err(Failure::Engine("engine missing after load".into()));
    };
    let vocabulary = if mode.use_vocabulary { config.vocabulary.terms_text() } else { Vec::new() };
    let options = TranscribeOptions { language: mode.language.as_deref(), vocabulary: &vocabulary };
    let started = Instant::now();
    match loaded.engine.transcribe(samples, &options) {
        Ok(text) if !text.trim().is_empty() => {
            Ok((text.trim().to_owned(), started.elapsed().as_millis() as u64, loaded.spec.id()))
        }
        Ok(_) => Err(Failure::Engine("no speech detected".into())),
        Err(e) => Err(Failure::Engine(e.to_string())),
    }
}

// TODO: refactor
/// führt den llm-schritt aus. liefert den finalen text, ein `backend/modell`-label, die verstrichenen millisekunden und im fehlerfall einen grund.
fn refine(
    mode: &Mode,
    text: &str,
    config: &Config,
    models_root: &Path,
    refiners: &mut Refiners,
) -> (String, Option<String>, Option<u64>, Option<String>) {
    let Some(refine) = mode.refine() else {
        return (text.to_owned(), None, None, None);
    };
    let mut refiner = match refiners.take(mode, config, models_root) {
        Ok(refiner) => refiner,
        Err(e) => return (text.to_owned(), None, None, Some(e)),
    };
    let vocabulary = if mode.use_vocabulary { config.vocabulary.terms_text() } else { Vec::new() };
    let system = prompt::system_prompt(mode, &vocabulary);
    let user = prompt::user_prompt(text);
    let timeout = Duration::from_millis(u64::from(if refine.timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        refine.timeout_ms
    }));
    let request = LlmRequest::new(&system, &user, refine.temperature, prompt::max_tokens(text), timeout);
    let started = Instant::now();
    let result = refiner.refine(&request);
    let elapsed = started.elapsed().as_millis() as u64;
    refiners.put(mode, refiner);
    match result {
        Ok(output) => {
            let cleaned = prompt::clean_output(&output, text);
            let label = refine
                .model
                .as_deref()
                .map(|model| format!("{}/{model}", refine.backend.as_str()))
                .unwrap_or_else(|| refine.backend.as_str());
            (cleaned, Some(label), Some(elapsed), None)
        }
        Err(e) => (text.to_owned(), None, Some(elapsed), Some(e.reason())),
    }
}

fn meeting_title(app_name: Option<&str>) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_secs()).unwrap_or(0);
    let seconds = now % 86_400;
    let stamp = format!("{:02}:{:02}", seconds / 3_600, (seconds % 3_600) / 60);
    match app_name {
        Some(name) if !name.is_empty() => format!("{name} call, {stamp}"),
        _ => format!("Meeting, {stamp}"),
    }
}

fn start_meeting(
    store: &Store,
    app: Option<String>,
    app_name: Option<String>,
    pids: Vec<i32>,
    config: &Config,
    meeting_tx: &Sender<MeetingMsg>,
    emit: &impl Fn(Event),
) -> Result<ActiveMeeting, String> {
    let started_at = now_ms();
    let title = meeting_title(app_name.as_deref());
    let id = store
        .insert_meeting(&NewMeeting {
            title: title.clone(),
            app_bundle_id: app.clone(),
            app_name: app_name.clone(),
            started_at,
        })
        .map_err(|error| format!("Could not start the meeting: {error}"))?;

    if config.meetings.keep_audio {
        let me = crate::paths::meeting_audio_file(id, "me");
        let them = crate::paths::meeting_audio_file(id, "them");
        if let Err(error) =
            store.set_meeting_audio(id, Some(&me.to_string_lossy()), Some(&them.to_string_lossy()))
        {
            crate::log_warn!("could not attach meeting audio paths: {error}");
        }
    }

    if let Some(uid) = config.audio.device.as_deref()
        && tf_meeting::coreaudio::input_device_by_uid(uid).is_none()
    {
        emit(Event::Notice(format!(
            "The selected microphone ({uid}) is not connected. Meetings use the system default."
        )));
    }

    let options = CaptureOptions {
        mic_uid: config.audio.device.clone(),
        app_pids: if config.meetings.capture == crate::config::CaptureMode::App { pids } else { Vec::new() },
        exclude_own_process: true,
    };
    let keep_audio = config.meetings.keep_audio;
    let identify_speakers = config.meetings.identify_speakers;
    let tx = meeting_tx.clone();
    let (stop, stop_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("tf-meeting".into())
        .spawn(move || meeting_thread(id, options, keep_audio, identify_speakers, stop_rx, tx))
        .map_err(|error| {
            let _ = store.set_meeting_status(id, MeetingStatus::Interrupted);
            format!("Could not start meeting capture: {error}")
        })?;

    Ok(ActiveMeeting { id, app_bundle_id: app, app_name, stop, thread: Some(thread) })
}

fn meeting_thread(
    id: MeetingId,
    options: CaptureOptions,
    keep_audio: bool,
    identify_speakers: bool,
    stop: Receiver<()>,
    tx: Sender<MeetingMsg>,
) {
    let started = Instant::now();
    let capture = match Capture::start(&options) {
        Ok(capture) => capture,
        Err(error) => {
            let _ =
                tx.send(MeetingMsg::Failed { id, message: format!("Could not capture the call: {error}") });
            return;
        }
    };
    if capture.echo_likely() {
        crate::log_info!("meetings: speakers detected, relying on the tap for the remote side");
    }

    let me_path = crate::paths::meeting_dir(id).join("me.m4a");
    let them_path = crate::paths::meeting_dir(id).join("them.m4a");
    let mut me_writer = if keep_audio { tf_audio::file::AacWriter::create(&me_path).ok() } else { None };
    // die remote-spur wird für die diarisierung gebraucht, auch wenn der nutzer kein meeting-audio behält. dann wird sie nach dem lauf gelöscht.
    let write_them = keep_audio || identify_speakers;
    let mut them_writer = if write_them { tf_audio::file::AacWriter::create(&them_path).ok() } else { None };
    let mut them_offset_ms: Option<u64> = None;
    let mut me_segmenter = Segmenter::new(Speaker::Me, 16_000);
    let mut them_segmenter = Segmenter::new(Speaker::Them, 16_000);
    let mut me_raw: Vec<f32> = Vec::new();
    let mut them_raw: Vec<f32> = Vec::new();
    let mut last_flush = Instant::now() - Duration::from_millis(MEETING_RESAMPLE_MS);

    loop {
        let stopping = match stop.recv_timeout(MEETING_DRAIN_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        me_raw.extend(capture.drain_me());
        them_raw.extend(capture.drain_them());
        if stopping || last_flush.elapsed() >= Duration::from_millis(MEETING_RESAMPLE_MS) {
            let end_ms = started.elapsed().as_millis() as u64;
            let them_closed = flush_speaker(
                &mut them_raw,
                capture.them_rate(),
                end_ms,
                &mut them_segmenter,
                them_writer.as_mut(),
            );
            let me_closed =
                flush_speaker(&mut me_raw, capture.me_rate(), end_ms, &mut me_segmenter, me_writer.as_mut());
            for segment in them_closed {
                send_meeting_audio(id, segment, &tx);
            }
            for segment in me_closed {
                if is_speaker_bleed(&segment, &them_segmenter, capture.echo_likely()) {
                    continue;
                }
                send_meeting_audio(id, segment, &tx);
            }
            if them_offset_ms.is_none() && them_writer.as_ref().is_some_and(|writer| writer.samples() > 0) {
                let written_ms = them_writer.as_ref().map(|writer| writer.samples() * 1_000 / 16_000);
                them_offset_ms = written_ms.map(|ms| end_ms.saturating_sub(ms));
            }
            last_flush = Instant::now();
        }
        if stopping {
            break;
        }
    }

    if let Some(segment) = me_segmenter.flush() {
        if !is_speaker_bleed(&segment, &them_segmenter, capture.echo_likely()) {
            send_meeting_audio(id, segment, &tx);
        }
    }
    if let Some(segment) = them_segmenter.flush() {
        send_meeting_audio(id, segment, &tx);
    }
    drop(capture);
    if let Some(writer) = me_writer.take() {
        writer.finish();
    }
    if let Some(writer) = them_writer.take() {
        writer.finish();
    }
    let _ = tx.send(MeetingMsg::Done { id, ended_at: now_ms(), them_offset_ms: them_offset_ms.unwrap_or(0) });
}

/// ein kurzes "Me"-segment mitten in der sprache der gegenseite ist durchsickern der speaker ins mikrofon. allein transkribiert wird daraus "Yeah" und "Mm-hmm".
fn is_speaker_bleed(segment: &tf_meeting::ClosedSegment, them: &Segmenter, echo_likely: bool) -> bool {
    if !echo_likely || segment.speaker != Speaker::Me {
        return false;
    }
    let duration = segment.end_ms.saturating_sub(segment.start_ms).max(1);
    let overlap = them.voiced_overlap_ms(segment.start_ms, segment.end_ms);
    duration < BLEED_MAX_SEGMENT_MS && overlap * 2 >= duration
}

fn flush_speaker(
    raw: &mut Vec<f32>,
    rate: u32,
    end_ms: u64,
    segmenter: &mut Segmenter,
    writer: Option<&mut tf_audio::file::AacWriter>,
) -> Vec<tf_meeting::ClosedSegment> {
    if raw.is_empty() {
        return Vec::new();
    }
    let samples = tf_audio::resample::to_16k(raw, rate);
    raw.clear();
    if samples.is_empty() {
        return Vec::new();
    }
    if let Some(writer) = writer {
        let _ = writer.write(&samples);
    }
    let duration_ms = samples.len() as u64 * 1000 / 16_000;
    let start_ms = end_ms.saturating_sub(duration_ms);
    segmenter.push(&samples, start_ms)
}

fn send_meeting_audio(meeting_id: MeetingId, segment: tf_meeting::ClosedSegment, tx: &Sender<MeetingMsg>) {
    let _ = tx.send(MeetingMsg::Audio(MeetingAudio {
        meeting_id,
        speaker: segment.speaker,
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
        samples: segment.samples,
    }));
}

#[allow(clippy::too_many_arguments)]
fn transcribe_meeting_audio(
    store: &Store,
    audio: MeetingAudio,
    engines: &mut Engines,
    engine_rx: &Receiver<EngineMsg>,
    load_tx: &Sender<EngineSpec>,
    config: &Config,
    emit: &impl Fn(Event),
) {
    if audio.samples.is_empty() {
        return;
    }
    let (text, _) = match transcribe_once(engines, engine_rx, load_tx, &audio.samples, emit) {
        Ok(value) => value,
        Err(error) => {
            crate::log_warn!("meeting segment transcription failed: {}", error.message());
            return;
        }
    };
    let text = config.vocabulary.apply(&text);
    if text.trim().is_empty() {
        return;
    }
    if duplicate_of_them(store, audio.meeting_id, audio.speaker, audio.start_ms, audio.end_ms, &text) {
        return;
    }
    let speaker = match audio.speaker {
        Speaker::Me => tf_store::Speaker::Me,
        Speaker::Them => tf_store::Speaker::Them,
    };
    match store.insert_segment(&NewSegment {
        meeting_id: audio.meeting_id,
        speaker,
        start_ms: audio.start_ms,
        end_ms: audio.end_ms,
        text: text.clone(),
    }) {
        Ok(_) => {
            emit(Event::MeetingSegment {
                meeting_id: audio.meeting_id,
                speaker: audio.speaker,
                start_ms: audio.start_ms,
                end_ms: audio.end_ms,
                text,
            });
        }
        Err(error) => crate::log_error!("could not store a meeting segment: {error}"),
    }
}

/// verwirft ein "Me"-segment, das eigentlich von der gegenseite kommt und ins mikrofon durchsickert: zeitlich überlappend und textlich fast identisch.
fn duplicate_of_them(
    store: &Store,
    meeting_id: MeetingId,
    speaker: Speaker,
    start_ms: u64,
    end_ms: u64,
    text: &str,
) -> bool {
    if speaker != Speaker::Me {
        return false;
    }
    let Ok(segments) = store.segments(meeting_id) else {
        return false;
    };
    let duration = end_ms.saturating_sub(start_ms).max(1);
    segments.iter().filter(|segment| segment.speaker == tf_store::Speaker::Them).any(|segment| {
        let overlap = end_ms.min(segment.end_ms).saturating_sub(start_ms.max(segment.start_ms));
        overlap * 2 > duration && text_similarity(text, &segment.text) >= 0.8
    })
}

fn text_similarity(left: &str, right: &str) -> f32 {
    let tokens = |text: &str| {
        text.to_lowercase()
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let left = tokens(left);
    let right = tokens(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let mut matched = 0usize;
    let mut used = vec![false; right.len()];
    for token in &left {
        if let Some(index) =
            right.iter().enumerate().position(|(index, other)| !used[index] && other == token)
        {
            used[index] = true;
            matched += 1;
        }
    }
    (2 * matched) as f32 / (left.len() + right.len()) as f32
}

fn diarization_models(models_root: &Path) -> PathBuf {
    models_root.join("speaker-diarization")
}

/// clustert die gespeicherten turn-embeddings neu, mit den vom nutzer fixierten turns als fixpunkten. namen und farben folgen den passenden speakern.
fn recluster_speakers(store: &Store, meeting_id: MeetingId, config: &Config, emit: &impl Fn(Event)) {
    let rows = store.segment_embeddings(meeting_id).unwrap_or_default();
    let previous = store.speaker_centroids(meeting_id).unwrap_or_default();
    let index_of = previous
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (*id, index))
        .collect::<std::collections::HashMap<_, _>>();
    let previous_vectors = previous.iter().map(|(_, vector)| vector.clone()).collect::<Vec<_>>();
    let embeddings = rows.iter().map(|row| row.3.clone()).collect::<Vec<_>>();
    let locked = rows
        .iter()
        .map(|row| row.2.then(|| row.1.and_then(|speaker| index_of.get(&speaker).copied())).flatten())
        .collect::<Vec<_>>();
    let options = tf_diarize::Options {
        threshold: config.meetings.speaker_threshold,
        max_speakers: usize::from(config.meetings.max_speakers.max(1)),
        ..tf_diarize::Options::default()
    };
    let labels = tf_diarize::recluster(&embeddings, &locked, &previous_vectors, &options);
    let moves = rows
        .iter()
        .zip(&labels)
        .zip(&locked)
        .filter(|(_, lock)| lock.is_none())
        .filter_map(|((row, label), _)| {
            let target = match (*label)? {
                label if label < previous.len() => SpeakerTarget::Existing(previous[label].0),
                label => SpeakerTarget::New(label),
            };
            Some((row.0, target))
        })
        .collect::<Vec<_>>();
    match store.reassign_segments(meeting_id, &moves) {
        Ok(()) => emit(Event::Notice("Speakers updated from the stored voice data.".into())),
        Err(error) => crate::log_error!("could not re-cluster speakers: {error}"),
    }
    emit(Event::HistoryChanged);
}

#[allow(clippy::too_many_arguments)]
fn rerun_diarization(
    store: &Store,
    meeting_id: MeetingId,
    config: &Config,
    models_root: &Path,
    job_tx: &Sender<Job>,
    emit: &impl Fn(Event),
    turn_queue: &Option<TurnQueue>,
    queue: &mut VecDeque<DiarizationRequest>,
    running: &mut Option<MeetingId>,
    cancel: &mut Arc<AtomicBool>,
) {
    let Some(meeting) = store.meeting(meeting_id).ok().flatten() else {
        return;
    };
    let path = crate::paths::meeting_dir(meeting_id).join("them.m4a");
    if !path.exists() {
        // ohne das audio lassen sich die gespeicherten stimmdaten noch neu clustern.
        if store.segment_embeddings(meeting_id).is_ok_and(|rows| rows.iter().any(|row| row.3.is_some())) {
            recluster_speakers(store, meeting_id, config, emit);
        } else {
            emit(Event::Notice("Speaker identification needs the call audio, which was deleted.".into()));
        }
        return;
    }
    if !diarization_models(models_root).join("Segmentation.mlmodelc").exists() {
        emit(Event::Notice("Install the Speaker identification model first.".into()));
        return;
    }
    let _ = store.set_diarization(meeting_id, Some(DiarizationState::Running));
    emit(Event::DiarizationStarted { id: meeting_id });
    queue.push_back(DiarizationRequest {
        meeting_id,
        path,
        offset_ms: meeting.them_offset_ms.unwrap_or(0),
        keep_audio: meeting.audio_them.is_some(),
    });
    if turn_queue.is_none() {
        maybe_start_diarization(queue, running, cancel, config, models_root, job_tx);
    }
}

fn maybe_start_diarization(
    queue: &mut VecDeque<DiarizationRequest>,
    running: &mut Option<MeetingId>,
    cancel: &mut Arc<AtomicBool>,
    config: &Config,
    models_root: &Path,
    job_tx: &Sender<Job>,
) {
    if running.is_some() {
        return;
    }
    let Some(request) = queue.pop_front() else {
        return;
    };
    let flag = Arc::new(AtomicBool::new(false));
    *cancel = Arc::clone(&flag);
    *running = Some(request.meeting_id);
    let options = tf_diarize::Options {
        threshold: config.meetings.speaker_threshold,
        max_speakers: usize::from(config.meetings.max_speakers.max(1)),
        ..tf_diarize::Options::default()
    };
    let models = models_root.to_path_buf();
    let job_tx = job_tx.clone();
    let meeting_id = request.meeting_id;
    let task: DiarizeTask = Box::new(move |cache| {
        let result = run_diarization(cache, &request.path, &models, &options, &job_tx, meeting_id, &flag);
        let _ = job_tx.send(Job::DiarizationReady {
            meeting_id,
            result,
            path: request.path,
            offset_ms: request.offset_ms,
            keep_audio: request.keep_audio,
        });
    });
    if diarize_thread().is_none_or(|sender| sender.send(task).is_err()) {
        crate::log_error!("could not start the diarization thread");
        *running = None;
    }
}

const DIARIZER_RESIDENCY: Duration = Duration::from_secs(5 * 60);

type DiarizeTask = Box<dyn FnOnce(&mut Option<tf_diarize::Diarizer>) + Send>;

/// ein langlebiger thread besitzt die diarisierungs-modelle, damit aufeinanderfolgende läufe das laden sparen, und gibt sie nach `DIARIZER_RESIDENCY` ohne arbeit frei.
fn diarize_thread() -> Option<&'static Sender<DiarizeTask>> {
    static THREAD: OnceLock<Option<Sender<DiarizeTask>>> = OnceLock::new();
    THREAD
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<DiarizeTask>();
            std::thread::Builder::new()
                .name("tf-diarize".into())
                .spawn(move || {
                    let mut cache = None;
                    loop {
                        match rx.recv_timeout(DIARIZER_RESIDENCY) {
                            Ok(task) => task(&mut cache),
                            Err(RecvTimeoutError::Timeout) => cache = None,
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

fn run_diarization(
    cache: &mut Option<tf_diarize::Diarizer>,
    path: &Path,
    models_root: &Path,
    options: &tf_diarize::Options,
    job_tx: &Sender<Job>,
    meeting_id: MeetingId,
    cancel: &AtomicBool,
) -> Result<tf_diarize::Diarization, String> {
    let samples =
        tf_audio::file::read(path).map_err(|error| format!("the call audio is missing: {error}"))?;
    if cache.is_none() {
        *cache = Some(tf_diarize::Diarizer::load(models_root).map_err(|error| error.to_string())?);
    }
    let diarizer = cache.as_mut().expect("loaded above");
    let progress_tx = job_tx.clone();
    let outcome = diarizer.run(
        &samples,
        options,
        move |fraction| {
            let _ = progress_tx.send(Job::DiarizationProgress { meeting_id, fraction });
        },
        cancel,
    );
    outcome.map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn accept_diarization_ready(
    meeting_id: MeetingId,
    result: Result<tf_diarize::Diarization, String>,
    path: PathBuf,
    offset_ms: u64,
    keep_audio: bool,
    history: Option<&Store>,
    config: &Config,
    models_root: &Path,
    job_tx: &Sender<Job>,
    emit: &impl Fn(Event),
    turn_queue: &mut Option<TurnQueue>,
    diarization_queue: &mut VecDeque<DiarizationRequest>,
    diarization_running: &mut Option<MeetingId>,
    diarization_cancel: &mut Arc<AtomicBool>,
) {
    *diarization_running = None;
    // das meeting kann gelöscht worden sein, während der lauf unterwegs war.
    let history = history.filter(|store| store.meeting(meeting_id).ok().flatten().is_some());
    if let Some(store) = history {
        match result {
            Ok(diarization) => {
                let existing = store
                    .segments(meeting_id)
                    .map(|segments| {
                        segments.iter().filter(|segment| segment.speaker == tf_store::Speaker::Them).count()
                    })
                    .unwrap_or(0);
                if diarization.turns.is_empty() && existing > 0 {
                    fail_diarization(
                        store,
                        meeting_id,
                        "no speech was detected in the call audio".into(),
                        config,
                        models_root,
                        job_tx,
                        emit,
                    );
                } else {
                    match tf_audio::file::read(&path) {
                        Ok(samples) => {
                            *turn_queue = Some(TurnQueue {
                                meeting_id,
                                path,
                                samples,
                                offset_ms,
                                keep_audio,
                                turns: diarization.turns,
                                centroids: diarization.centroids,
                                next: 0,
                                segments: Vec::new(),
                            });
                        }
                        Err(error) => fail_diarization(
                            store,
                            meeting_id,
                            format!("the call audio is missing: {error}"),
                            config,
                            models_root,
                            job_tx,
                            emit,
                        ),
                    }
                }
            }
            Err(message) => fail_diarization(store, meeting_id, message, config, models_root, job_tx, emit),
        }
    }
    // nur ein lauf und eine turn-queue gleichzeitig; der nächste lauf startet, nachdem die turns des vorherigen meetings geschrieben sind.
    if turn_queue.is_none() {
        maybe_start_diarization(
            diarization_queue,
            diarization_running,
            diarization_cancel,
            config,
            models_root,
            job_tx,
        );
    }
}

fn fail_diarization(
    store: &Store,
    meeting_id: MeetingId,
    message: String,
    config: &Config,
    models_root: &Path,
    job_tx: &Sender<Job>,
    emit: &impl Fn(Event),
) {
    crate::log_warn!("speaker identification failed for meeting {meeting_id}: {message}");
    let _ = store.set_diarization(meeting_id, Some(DiarizationState::Failed));
    if store.meeting(meeting_id).ok().flatten().is_some_and(|meeting| meeting.audio_them.is_none()) {
        let _ = fs::remove_file(crate::paths::meeting_dir(meeting_id).join("them.m4a"));
    }
    emit(Event::DiarizationFailed { id: meeting_id, message });
    emit(Event::HistoryChanged);
    spawn_summary(store, meeting_id, config, models_root, job_tx, emit);
}

fn finish_turn_queue(
    store: &Store,
    queue: TurnQueue,
    config: &Config,
    models_root: &Path,
    job_tx: &Sender<Job>,
    emit: &impl Fn(Event),
) {
    let previous = store.speaker_centroids(queue.meeting_id).unwrap_or_default();
    let previous_vectors = previous.iter().map(|(_, vector)| vector.clone()).collect::<Vec<_>>();
    let matches =
        tf_diarize::match_clusters(&queue.centroids, &previous_vectors, config.meetings.speaker_threshold)
            .into_iter()
            .map(|slot| slot.and_then(|index| previous.get(index).map(|(id, _)| *id)))
            .collect::<Vec<_>>();
    let result = DiarizedTranscript { segments: queue.segments, centroids: queue.centroids, matches };
    match store.replace_them_segments(queue.meeting_id, &result) {
        Ok(()) => {
            if !queue.keep_audio {
                let _ = fs::remove_file(&queue.path);
            }
            emit(Event::DiarizationFinished { id: queue.meeting_id });
            emit(Event::HistoryChanged);
            spawn_summary(store, queue.meeting_id, config, models_root, job_tx, emit);
        }
        Err(error) => {
            fail_diarization(
                store,
                queue.meeting_id,
                format!("could not save the speakers: {error}"),
                config,
                models_root,
                job_tx,
                emit,
            );
        }
    }
}

fn apply_speaker_edit(store: &Store, edit: SpeakerEdit, emit: &impl Fn(Event)) {
    let result = match edit {
        SpeakerEdit::Rename { speaker_id, name } => store.rename_speaker(speaker_id, name.as_deref()),
        SpeakerEdit::Assign { segment_ids, speaker_id } => store.assign_segments(&segment_ids, speaker_id),
        SpeakerEdit::NewSpeaker { segment_ids } => store.new_speaker_for(&segment_ids).map(|_| ()),
        SpeakerEdit::Merge { from, into } => store.merge_speakers(from, into),
        SpeakerEdit::Split { speaker_id, groups } => store.split_speaker(speaker_id, &groups).map(|_| ()),
    };
    if let Err(error) = result {
        crate::log_error!("could not apply the speaker correction: {error}");
    }
    emit(Event::HistoryChanged);
}

fn spawn_summary(
    store: &Store,
    meeting_id: MeetingId,
    config: &Config,
    models_root: &Path,
    job_tx: &Sender<Job>,
    emit: &impl Fn(Event),
) {
    let segments = store.segments(meeting_id).unwrap_or_default();
    if segments.is_empty() {
        let _ = store.set_meeting_status(meeting_id, MeetingStatus::Done);
        emit(Event::Notice("The meeting had no transcribable speech.".into()));
        emit(Event::HistoryChanged);
        return;
    }
    let mut refine = config.meetings.summary.clone();
    if refine.prompt.trim().is_empty() {
        refine.prompt = crate::mode::MEETING_SUMMARY_PROMPT.to_owned();
    }
    let mode = Mode { id: "summary".to_owned(), refine: Some(refine), ..Mode::default() };
    let speakers = store.meeting_speakers(meeting_id).unwrap_or_default();
    let labels = speakers
        .iter()
        .map(|speaker| (speaker.id, speaker.label()))
        .collect::<std::collections::HashMap<_, _>>();
    let labels = segments
        .iter()
        .map(|segment| {
            if segment.speaker == tf_store::Speaker::Me {
                "Me".to_owned()
            } else {
                segment
                    .speaker_id
                    .and_then(|id| labels.get(&id).cloned())
                    .unwrap_or_else(|| "Them".to_owned())
            }
        })
        .collect::<Vec<_>>();
    let lines = segments
        .iter()
        .zip(&labels)
        .map(|(segment, label)| summary::Line {
            speaker: label.as_str(),
            start_ms: segment.start_ms,
            text: &segment.text,
        })
        .collect::<Vec<_>>();
    let transcript = summary::transcript(lines);
    let config = config.clone();
    let models_root = models_root.to_path_buf();
    let job_tx = job_tx.clone();
    let mode_id = mode.id.clone();
    let spawned = std::thread::Builder::new().name("tf-meeting-summary".into()).spawn(move || {
        let result = summarize(&mode, &config, &models_root, &transcript);
        let _ = job_tx.send(Job::MeetingSummaryReady { meeting_id, result, mode: mode_id });
    });
    if spawned.is_err() {
        let _ = store.set_meeting_status(meeting_id, MeetingStatus::Done);
        emit(Event::Notice("Could not start the meeting summary.".into()));
    }
}

fn summarize(mode: &Mode, config: &Config, models_root: &Path, transcript: &str) -> Result<String, String> {
    let refine = mode.refine().ok_or_else(|| "the mode has no prompt".to_owned())?;
    let spec = refiner_spec(mode, config, models_root)?;
    let mut refiner = tf_llm::build(spec).map_err(|error| error.to_string())?;
    refiner.warm();
    let budget_tokens = match &refine.backend {
        Backend::Apple => 2_500,
        _ => config.llm.local.context.saturating_sub(1_024).max(1_024),
    };
    let budget_chars = (budget_tokens as usize).saturating_mul(4);
    let system = summary::final_system_prompt(&refine.prompt);
    let output = if transcript.chars().count() <= budget_chars {
        run_refiner(
            &mut *refiner,
            &system,
            &summary::final_user_prompt(transcript),
            refine.temperature,
            2_000,
        )?
    } else {
        let pieces = split_transcript(transcript, budget_chars / 2);
        let mut notes = String::new();
        for piece in pieces {
            let note = run_refiner(
                &mut *refiner,
                &summary::map_system_prompt(),
                &summary::map_user_prompt(&piece),
                0.2,
                700,
            )?;
            notes.push_str(note.trim());
            notes.push_str("\n\n");
        }
        run_refiner(&mut *refiner, &system, &summary::final_user_prompt(&notes), refine.temperature, 2_000)?
    };
    Ok(prompt::clean_output(&output, transcript))
}

fn run_refiner(
    refiner: &mut dyn Refiner,
    system: &str,
    user: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<String, String> {
    let request = LlmRequest::new(system, user, temperature, max_tokens, Duration::from_secs(180));
    refiner.refine(&request).map_err(|error| error.reason())
}

fn split_transcript(transcript: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(400);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in transcript.lines() {
        if !current.is_empty() && current.len() + line.len() + 1 > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

fn delete_meetings(store: &Store, ids: &[MeetingId], emit: &impl Fn(Event)) {
    match store.delete_meetings(ids) {
        Ok(paths) => {
            remove_support_files(&paths);
            for id in ids {
                let _ = fs::remove_dir_all(crate::paths::meeting_dir(*id));
            }
            emit(Event::HistoryChanged);
        }
        Err(error) => crate::log_error!("could not delete meetings: {error}"),
    }
}

fn remove_support_files(paths: &[PathBuf]) {
    let root = crate::paths::support_dir();
    for path in paths {
        let path = if path.is_absolute() { path.clone() } else { root.join(path) };
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("could not delete {}: {error}", path.display());
        }
    }
}

fn open_history(_config: &Config) -> Option<Store> {
    match Store::open(&crate::paths::history_path()) {
        Ok(store) => Some(store),
        Err(e) => {
            crate::log_error!("could not open the history database: {e}");
            None
        }
    }
}

/// öffnet die datenbank für ein explizites löschen, wenn history sonst `Off` ist, der nutzer aber seine alten zeilen behalten hat.
fn temporary_store() -> Option<Store> {
    let path = crate::paths::history_path();
    if !path.exists() {
        return None;
    }
    match Store::open(&path) {
        Ok(store) => Some(store),
        Err(e) => {
            crate::log_error!("could not open the history database: {e}");
            None
        }
    }
}

fn write_history(
    pending: Pending,
    outcome: StoreOutcome,
    store: Option<&Store>,
    config: &Config,
    emit: &impl Fn(Event),
) {
    let Some(store) = store else {
        return;
    };
    if !config.history.store.stores_text() {
        return;
    }
    let take = pending.take;
    let new_take = NewTake {
        created_at: pending.created_at,
        duration_ms: take.duration_ms,
        app_bundle_id: take.app_bundle_id,
        app_name: take.app_name,
        mode_id: Some(take.mode.clone()),
        mode_name: Some(take.mode_name.clone()),
        engine: take.engine,
        language: None,
        raw_text: take.raw_text,
        final_text: take.final_text,
        llm: take.llm,
        stt_ms: Some(take.stt_ms),
        llm_ms: take.llm_ms,
        audio_file: None,
        outcome,
        parent_id: None,
    };
    match store.insert(&new_take) {
        Ok(id) => {
            if let Some(samples) = pending.samples {
                let relative = crate::paths::audio_file(pending.created_at, id);
                let path = crate::paths::audio_dir().join(&relative);
                match tf_audio::file::write_m4a(&path, &samples) {
                    Ok(()) => {
                        if let Err(e) = store.set_audio(id, &relative.to_string_lossy()) {
                            crate::log_warn!("could not attach audio to take {id}: {e}");
                        }
                    }
                    Err(e) => crate::log_warn!("could not write take audio: {e}"),
                }
            }
            emit(Event::HistoryChanged);
        }
        Err(e) => crate::log_error!("could not write history: {e}"),
    }
}

fn delete_takes(store: &Store, ids: &[i64], emit: &impl Fn(Event)) {
    match store.delete(ids) {
        Ok(paths) => {
            remove_audio(&paths);
            emit(Event::HistoryChanged);
        }
        Err(e) => crate::log_error!("could not delete history rows: {e}"),
    }
}

fn delete_all_history(store: &Store, emit: &impl Fn(Event)) {
    match store.delete_all() {
        Ok(paths) => {
            remove_audio(&paths);
            let _ = fs::remove_dir_all(crate::paths::audio_dir());
            match store.delete_all_meetings() {
                Ok(meeting_paths) => remove_support_files(&meeting_paths),
                Err(error) => crate::log_warn!("could not clear meetings: {error}"),
            }
            let _ = fs::remove_dir_all(crate::paths::meetings_dir());
            emit(Event::HistoryChanged);
        }
        Err(e) => crate::log_error!("could not clear history: {e}"),
    }
}

fn delete_all_audio(store: &Store, emit: &impl Fn(Event)) {
    let takes = store.delete_all_audio().unwrap_or_default();
    let meetings = store.delete_all_meeting_audio().unwrap_or_default();
    remove_audio(&takes);
    remove_support_files(&meetings);
    let _ = fs::remove_dir_all(crate::paths::audio_dir());
    let _ = fs::remove_dir_all(crate::paths::meetings_dir());
    emit(Event::HistoryChanged);
}

fn prune_history(store: &Store, config: &Config, emit: &impl Fn(Event)) {
    let mut retention = tf_store::Retention {
        text_days: config.history.retention_days,
        audio_days: config.history.audio_retention_days,
    };
    if let (Some(text), Some(audio)) = (retention.text_days, retention.audio_days)
        && audio > text
    {
        retention.audio_days = Some(text);
    }
    let mut changed = false;
    match store.prune(now_ms(), &retention) {
        Ok(pruned) => {
            if !pruned.deleted_ids.is_empty() || !pruned.audio_files.is_empty() {
                remove_audio(&pruned.audio_files);
                changed = true;
            }
        }
        Err(e) => crate::log_error!("could not prune history: {e}"),
    }
    if let Ok(paths) = store.prune_meeting_audio(now_ms(), retention.audio_days)
        && !paths.is_empty()
    {
        remove_support_files(&paths);
        changed = true;
    }
    if changed {
        emit(Event::HistoryChanged);
    }
}

#[allow(clippy::too_many_arguments)]
fn rerun(
    store: &Store,
    take_id: i64,
    engine: Option<String>,
    models_root: &Path,
    engines: &mut Engines,
    engine_rx: &Receiver<EngineMsg>,
    load_tx: &Sender<EngineSpec>,
    config: &Config,
    emit: &impl Fn(Event),
) {
    let parent = match store.get(take_id) {
        Ok(Some(take)) => take,
        Ok(None) => return,
        Err(e) => {
            crate::log_error!("could not read take {take_id}: {e}");
            return;
        }
    };
    let Some(relative) = parent.audio_file.clone() else {
        emit(Event::Notice("This take has no audio to re-run.".into()));
        return;
    };
    let source = crate::paths::audio_dir().join(&relative);
    let samples = match tf_audio::file::read(&source) {
        Ok(samples) if !samples.is_empty() => samples,
        Ok(_) => {
            emit(Event::Notice("The stored audio is empty.".into()));
            return;
        }
        Err(e) => {
            crate::log_error!("could not read take audio: {e}");
            emit(Event::Failed(Failure::Audio(e.to_string())));
            return;
        }
    };

    let original = engines.spec.clone();
    let spec = engine.map(|id| EngineSpec::parse(&id, models_root)).unwrap_or_else(|| original.clone());
    let switched = spec != original;
    if switched {
        engines.set_spec(spec.clone(), load_tx, emit);
    }
    let result = transcribe_once(engines, engine_rx, load_tx, &samples, emit);
    if switched {
        engines.set_spec(original, load_tx, emit);
    }

    let (text, stt_ms) = match result {
        Ok(value) => value,
        Err(failure) => {
            emit(Event::Failed(failure));
            return;
        }
    };
    let created_at = now_ms();
    let duration_ms = samples.len() as u64 * 1000 / u64::from(tf_stt::SAMPLE_RATE);
    let new_take = NewTake {
        created_at,
        duration_ms,
        app_bundle_id: parent.app_bundle_id.clone(),
        app_name: parent.app_name.clone(),
        mode_id: parent.mode_id.clone(),
        mode_name: parent.mode_name.clone(),
        engine: spec.id(),
        language: parent.language.clone(),
        raw_text: text.clone(),
        final_text: text,
        llm: None,
        stt_ms: Some(stt_ms),
        llm_ms: None,
        audio_file: None,
        outcome: StoreOutcome::Rerun,
        parent_id: Some(take_id),
    };
    match store.insert(&new_take) {
        Ok(id) => {
            if config.history.store.stores_audio() {
                let target = crate::paths::audio_file(created_at, id);
                let destination = crate::paths::audio_dir().join(&target);
                let copied = destination
                    .parent()
                    .map(fs::create_dir_all)
                    .transpose()
                    .and_then(|_| fs::copy(&source, &destination));
                match copied {
                    Ok(_) => {
                        if let Err(e) = store.set_audio(id, &target.to_string_lossy()) {
                            crate::log_warn!("could not attach re-run audio: {e}");
                        }
                    }
                    Err(e) => crate::log_warn!("could not copy re-run audio: {e}"),
                }
            }
            emit(Event::HistoryChanged);
            emit(Event::RerunDone(id));
        }
        Err(e) => crate::log_error!("could not record the re-run: {e}"),
    }
}

fn transcribe_once(
    engines: &mut Engines,
    engine_rx: &Receiver<EngineMsg>,
    load_tx: &Sender<EngineSpec>,
    samples: &[f32],
    emit: &impl Fn(Event),
) -> Result<(String, u64), Failure> {
    engines.ensure(engine_rx, load_tx, emit)?;
    let Some(loaded) = engines.loaded.as_mut() else {
        return Err(Failure::Engine("engine missing after load".into()));
    };
    let started = Instant::now();
    match loaded.engine.transcribe(samples, &TranscribeOptions::default()) {
        Ok(text) if !text.trim().is_empty() => {
            Ok((text.trim().to_owned(), started.elapsed().as_millis() as u64))
        }
        Ok(_) => Err(Failure::Engine("no speech detected".into())),
        Err(e) => Err(Failure::Engine(e.to_string())),
    }
}

fn remove_audio(paths: &[PathBuf]) {
    let root = crate::paths::audio_dir();
    for path in paths {
        let path = if path.is_absolute() { path.clone() } else { root.join(path) };
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("could not delete {}: {e}", path.display());
        }
    }
}

fn sweep_audio(store: &Store) {
    let known = match store.audio_files() {
        Ok(files) => files,
        Err(e) => {
            crate::log_warn!("could not list history audio: {e}");
            return;
        }
    };
    let root = crate::paths::audio_dir();
    let mut files = Vec::new();
    collect_files(&root, &mut files);
    for path in files {
        let relative = path.strip_prefix(&root).map(|path| path.to_string_lossy().to_string());
        let orphan = match relative {
            Ok(relative) if path.extension().is_some_and(|ext| ext == "m4a") => !known.contains(&relative),
            Ok(_) => path.extension().is_some_and(|ext| ext == "tmp"),
            Err(_) => false,
        };
        if orphan {
            let _ = fs::remove_file(&path);
        }
    }

    let known = store.meeting_audio_files().unwrap_or_default();
    let root = crate::paths::support_dir();
    let mut files = Vec::new();
    collect_files(&crate::paths::meetings_dir(), &mut files);
    for path in files {
        let Ok(relative) = path.strip_prefix(&root) else {
            continue;
        };
        let relative = relative.to_string_lossy().to_string();
        if path.extension().is_some_and(|ext| ext == "m4a") && !known.contains(&relative) {
            let _ = fs::remove_file(&path);
        }
    }
}

fn collect_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect_files(&path, out),
            Ok(kind) if kind.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as i64).unwrap_or(0)
}

fn load_engine(spec: EngineSpec, tx: Sender<EngineMsg>) {
    let start = Instant::now();
    let message = match tf_stt::load(&spec) {
        Ok(mut engine) => {
            if spec.is_parakeet() {
                let silence = vec![0.0f32; tf_stt::SAMPLE_RATE as usize];
                if let Err(e) = engine.transcribe(&silence, &TranscribeOptions::default()) {
                    let _ = tx.send(EngineMsg::Failed { spec, failure: stt_failure(e) });
                    return;
                }
            }
            crate::log_info!("speech engine {} ready in {} ms", spec.id(), start.elapsed().as_millis());
            EngineMsg::Ready { spec, engine }
        }
        Err(e) => {
            crate::log_error!("could not load the speech engine: {e}");
            EngineMsg::Failed { spec, failure: stt_failure(e) }
        }
    };
    let _ = tx.send(message);
}

fn stt_failure(error: tf_stt::Error) -> Failure {
    match error {
        tf_stt::Error::MissingModel(path) => Failure::ModelMissing(path),
        other => Failure::Engine(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::preset;

    #[test]
    fn apple_modes_resolve_to_the_apple_spec() {
        let config = Config::default();
        let mode = preset("clean").unwrap();
        assert!(matches!(refiner_spec(&mode, &config, Path::new("/models")), Ok(tf_llm::Spec::Apple)));
    }

    #[test]
    fn raw_modes_have_no_refiner_spec() {
        let config = Config::default();
        assert!(refiner_spec(&preset("raw").unwrap(), &config, Path::new("/models")).is_err());
    }

    #[test]
    fn unknown_endpoints_are_rejected() {
        let config = Config::default();
        let mut mode = preset("clean").unwrap();
        mode.refine.as_mut().unwrap().backend = Backend::Endpoint("missing".into());
        let error = refiner_spec(&mode, &config, Path::new("/models")).unwrap_err();
        assert!(error.contains("unknown endpoint"));
    }

    #[test]
    fn missing_local_models_are_reported() {
        let config = Config::default();
        let mut mode = preset("clean").unwrap();
        let refine = mode.refine.as_mut().unwrap();
        refine.backend = Backend::Local;
        refine.model = Some("not-installed".into());
        let error = refiner_spec(&mode, &config, Path::new("/definitely/not/models")).unwrap_err();
        assert!(error.contains("not installed"));
    }

    #[test]
    fn gguf_search_walks_nested_directories() {
        let root = std::env::temp_dir().join(format!("tf-gguf-{}", std::process::id()));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("model.gguf"), b"x").unwrap();
        assert_eq!(find_gguf(&root), Some(nested.join("model.gguf")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn echo_similarity_is_conservative() {
        assert!(text_similarity("Hello there, can you hear me?", "hello there can you hear me") > 0.9);
        assert!(text_similarity("Yes, loud and clear", "yes loud and clear") > 0.9);
        assert!(text_similarity("Let's start with the roadmap", "I'll send the report") < 0.2);
        assert_eq!(text_similarity("", "anything"), 0.0);
    }
}
