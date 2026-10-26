use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message as ObjcMessage, define_class,
    msg_send, sel,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationOptions,
    NSApplicationActivationPolicy, NSApplicationDelegate, NSColor, NSEventModifierFlags, NSImage, NSMenu,
    NSMenuItem, NSModalResponseOK, NSRunningApplication, NSSavePanel, NSSound, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength, NSWindow, NSWindowDelegate, NSWorkspace,
};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_avf_audio::AVAudioPlayer;
use objc2_core_graphics::{CGPreflightListenEventAccess, CGPreflightPostEventAccess};
use objc2_foundation::{
    NSArray, NSBundle, NSDateFormatter, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSTimer, NSURL, ns_string,
};
#[allow(deprecated)]
use objc2_foundation::{
    NSUserNotification, NSUserNotificationAction, NSUserNotificationActivationType, NSUserNotificationCenter,
    NSUserNotificationCenterDelegate,
};
use tf_core::config::{Config, SoundStyle};
use tf_core::mode::{ModeId, RAW_ID};
use tf_core::paths::{audio_dir, config_path, history_path, model_dir, models_dir, support_dir};
use tf_core::trigger::{HotkeyEvent, Machine};
use tf_core::{Action, Core, Event, Failure, Input, Take};
use tf_meeting::{DetectedApp, Detector};
use tf_models::runtime_root;
use tf_models::{Catalog, Progress as ModelProgress};
use tf_store::{MeetingId, Outcome as StoreOutcome, Query, Store, TakeId};

use crate::history::History;
use crate::hotkey;
use crate::insert::{self, Outcome};
use crate::live::LiveTranscript;
use crate::pill::{Animation, Pill};
use crate::settings::Settings;
use crate::shell::Page;

const SAMPLE_MAX_MS: u64 = 4_000;
const FRAME_INTERVAL: f64 = 1.0 / 60.0;
const PILL_DELAY: Duration = Duration::from_millis(120);
const ERROR_FLASH: Duration = Duration::from_millis(400);
const EMPTY_SHAKE: Duration = Duration::from_millis(250);
const FADE: f64 = 0.12;
const WAVE: f64 = std::f64::consts::TAU;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DownloadPhase {
    Downloading,
    Verifying,
    Optimizing,
    Installed,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct DownloadView {
    pub id: String,
    pub progress: f64,
    pub total: u64,
    pub phase: DownloadPhase,
}

enum SpeechInstallMsg {
    Progress(f64),
    Done(Result<(), String>),
}

/// events gehen an den main-thread; `Level` läuft lock-free im callback.
pub enum Message {
    Core(Event),
    Hotkey { trigger: Option<ModeId>, event: HotkeyEvent },
    HotkeyModeSwitch { back: bool },
    HotkeyError(String),
}

/// schickt arbeit auf die main-queue. der delegate gehört `main` für die ganze laufzeit von `NSApplication::run`, darum bleibt der pointer gültig.
#[derive(Clone, Copy)]
pub struct MainDispatcher {
    delegate: *mut AppDelegate,
}

// SAFETY: die closure läuft nur auf der main-queue und dereferenziert dort den
unsafe impl Send for MainDispatcher {}
unsafe impl Sync for MainDispatcher {}

impl MainDispatcher {
    fn new(delegate: &AppDelegate) -> Self {
        Self { delegate: delegate as *const AppDelegate as *mut AppDelegate }
    }

    pub fn send(self, message: Message) {
        dispatch2::DispatchQueue::main().exec_async(move || self.deliver(message));
    }

    fn deliver(self, message: Message) {
        // SAFETY: siehe kommentar am typ.
        let delegate = unsafe { &*self.delegate };
        delegate.handle(message);
    }
}

enum Phase {
    Idle,
    Recording,
    Processing,
    Error { since: Instant },
    Empty { since: Instant },
    Fading { since: Instant },
}

#[derive(Clone, Copy)]
enum Sound {
    Start,
    Stop,
    Error,
}

pub struct State {
    status_item: OnceCell<Retained<NSStatusItem>>,
    toggle_item: OnceCell<Retained<NSMenuItem>>,
    status_line: OnceCell<Retained<NSMenuItem>>,
    pill: OnceCell<Pill>,
    core: OnceCell<Core>,
    config: RefCell<Config>,
    trigger: RefCell<Machine>,
    mode_machines: RefCell<BTreeMap<Option<ModeId>, Machine>>,
    hotkey_triggers: hotkey::Triggers,
    mode_switch_key: hotkey::SwitchKey,
    hotkey_paused: Arc<AtomicBool>,
    current_mode: RefCell<ModeId>,
    active_trigger: RefCell<Option<Option<ModeId>>>,
    mode_label_until: Cell<Option<Instant>>,
    phase: RefCell<Phase>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    clock: Cell<Instant>,
    anim_start: Cell<Instant>,
    show_at: Cell<Option<Instant>>,
    pill_shown: Cell<bool>,
    level: Arc<AtomicU32>,
    intercept_esc: Arc<AtomicBool>,
    engine_ready: Cell<bool>,
    engine_failure: RefCell<Option<Failure>>,
    tap_failed: Cell<bool>,
    last_take: RefCell<Option<Take>>,
    sounds: RefCell<Vec<Retained<NSSound>>>,
    settings: RefCell<Option<Settings>>,
    recent_menu: OnceCell<Retained<NSMenu>>,
    mode_menu: OnceCell<Retained<NSMenu>>,
    mode_list: RefCell<Vec<ModeId>>,
    player: RefCell<Option<Retained<AVAudioPlayer>>>,
    model_download: RefCell<Option<(tf_models::Download, Receiver<ModelProgress>)>>,
    speech_install: RefCell<Option<Receiver<SpeechInstallMsg>>>,
    download_state: RefCell<Option<DownloadView>>,
    download_timer: RefCell<Option<Retained<NSTimer>>>,
    live: OnceCell<LiveTranscript>,
    meeting_id: Cell<Option<MeetingId>>,
    meeting_app: RefCell<Option<String>>,
    meeting_started: Cell<Option<Instant>>,
    detector: RefCell<Detector>,
    detected_app: RefCell<Option<DetectedApp>>,
    detect_timer: RefCell<Option<Retained<NSTimer>>>,
    meeting_item: OnceCell<Retained<NSMenuItem>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfAppDelegate"]
    #[ivars = State]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.launch();
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            if let Some(core) = self.ivars().core.get() {
                core.stop_meeting();
                core.cancel();
            }
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            let closing = notification.object();
            let closing = closing.as_deref().and_then(|object| object.downcast_ref::<NSWindow>());
            let settings_closed = closing
                .and_then(|window| {
                    self.ivars()
                        .settings
                        .borrow()
                        .as_ref()
                        .map(|settings| std::ptr::eq(settings.window(), window))
                })
                .unwrap_or(false);
            if settings_closed && let Some(settings) = self.ivars().settings.borrow().as_ref() {
                settings.stop();
            }
        }
    }

    #[allow(deprecated)]
    unsafe impl NSUserNotificationCenterDelegate for AppDelegate {
        #[unsafe(method(userNotificationCenter:didActivateNotification:))]
        fn user_notification_activated(
            &self,
            _center: &NSUserNotificationCenter,
            notification: &NSUserNotification,
        ) {
            let Some(identifier) = notification.identifier() else {
                return;
            };
            if identifier.to_string() != "tf-meeting-start" {
                return;
            }
            let action = notification.activationType();
            if action == NSUserNotificationActivationType::ActionButtonClicked {
                self.start_detected_meeting();
            } else if action == NSUserNotificationActivationType::AdditionalActionClicked
                && notification
                    .additionalActivationAction()
                    .and_then(|action| action.identifier())
                    .is_some_and(|identifier| identifier.to_string() == "tf-meeting-not-now")
            {
                self.dismiss_detected_meeting();
            }
        }
    }

    impl AppDelegate {
        #[unsafe(method(toggleDictation:))]
        fn toggle_dictation(&self, _sender: Option<&AnyObject>) {
            let idle = matches!(*self.ivars().phase.borrow(), Phase::Idle);
            if idle {
                self.start_dictation(None);
            } else if let Some(core) = self.ivars().core.get() {
                core.stop();
            }
        }

        #[unsafe(method(showSettings:))]
        fn show_settings_action(&self, _sender: Option<&AnyObject>) {
            self.show_settings(Page::General);
        }

        #[unsafe(method(showHistory:))]
        fn show_history_action(&self, _sender: Option<&AnyObject>) {
            self.show_history();
        }

        #[unsafe(method(showMeetingHistory:))]
        fn show_meeting_history_action(&self, _sender: Option<&AnyObject>) {
            self.show_meeting_history();
        }

        #[unsafe(method(selectMode:))]
        fn select_mode(&self, sender: Option<&AnyObject>) {
            let Some(index) = sender
                .and_then(|sender| sender.downcast_ref::<NSMenuItem>())
                .map(|item| item.tag())
            else {
                return;
            };
            let Some(id) = self.ivars().mode_list.borrow().get(index as usize).cloned() else {
                return;
            };
            if self.ivars().config.borrow().default_mode_id == id {
                return;
            }
            self.update_config(|config| config.default_mode_id = id);
        }

        #[unsafe(method(editModes:))]
        fn edit_modes(&self, _sender: Option<&AnyObject>) {
            self.show_settings(Page::Modes);
        }

        #[unsafe(method(pasteLastTranscript:))]
        fn paste_last_transcript(&self, _sender: Option<&AnyObject>) {
            let Some(take) = self.ivars().last_take.borrow().clone() else {
                return;
            };
            self.insert_text(&take.final_text);
        }

        #[unsafe(method(copyRecent:))]
        fn copy_recent(&self, sender: Option<&AnyObject>) {
            let Some(id) = sender.and_then(|sender| sender.downcast_ref::<NSMenuItem>()).map(|item| item.tag())
            else {
                return;
            };
            let Ok(store) = Store::open_read_only(&history_path()) else {
                return;
            };
            let Ok(Some(take)) = store.get(id as TakeId) else {
                return;
            };
            insert::copy(&take.final_text);
        }

        #[unsafe(method(revealModels:))]
        fn reveal_models(&self, _sender: Option<&AnyObject>) {
            let dir = models_dir();
            let fallback = tf_core::paths::support_dir();
            open_path(if dir.exists() { &dir } else { &fallback });
        }

        #[unsafe(method(startMeeting:))]
        fn start_meeting_action(&self, _sender: Option<&AnyObject>) {
            self.start_meeting_now();
        }

        #[unsafe(method(stopMeeting:))]
        fn stop_meeting_action(&self, _sender: Option<&AnyObject>) {
            self.stop_meeting();
        }
    }
);

impl AppDelegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(State {
            status_item: OnceCell::new(),
            toggle_item: OnceCell::new(),
            status_line: OnceCell::new(),
            pill: OnceCell::new(),
            core: OnceCell::new(),
            config: RefCell::new(Config::default()),
            trigger: RefCell::new(Machine::new(tf_core::trigger::Style::Hybrid, 300)),
            mode_machines: RefCell::new(BTreeMap::new()),
            hotkey_triggers: Arc::new(RwLock::new(Vec::new())),
            mode_switch_key: Arc::new(RwLock::new(None)),
            hotkey_paused: Arc::new(AtomicBool::new(false)),
            current_mode: RefCell::new(RAW_ID.to_owned()),
            active_trigger: RefCell::new(None),
            mode_label_until: Cell::new(None),
            phase: RefCell::new(Phase::Idle),
            timer: RefCell::new(None),
            clock: Cell::new(Instant::now()),
            anim_start: Cell::new(Instant::now()),
            show_at: Cell::new(None),
            pill_shown: Cell::new(false),
            level: Arc::new(AtomicU32::new(0.0f32.to_bits())),
            intercept_esc: Arc::new(AtomicBool::new(false)),
            engine_ready: Cell::new(false),
            engine_failure: RefCell::new(None),
            tap_failed: Cell::new(false),
            last_take: RefCell::new(None),
            sounds: RefCell::new(Vec::new()),
            settings: RefCell::new(None),
            recent_menu: OnceCell::new(),
            mode_menu: OnceCell::new(),
            mode_list: RefCell::new(Vec::new()),
            player: RefCell::new(None),
            model_download: RefCell::new(None),
            speech_install: RefCell::new(None),
            download_state: RefCell::new(None),
            download_timer: RefCell::new(None),
            live: OnceCell::new(),
            meeting_id: Cell::new(None),
            meeting_app: RefCell::new(None),
            meeting_started: Cell::new(None),
            detector: RefCell::new(Detector::new()),
            detected_app: RefCell::new(None),
            detect_timer: RefCell::new(None),
            meeting_item: OnceCell::new(),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub fn install(&self, app: &NSApplication) {
        app.setDelegate(Some(ProtocolObject::from_ref(self)));
    }

    pub(crate) fn config(&self) -> Config {
        self.ivars().config.borrow().clone()
    }

    pub(crate) fn update_config(&self, change: impl FnOnce(&mut Config)) {
        let config = {
            let mut config = self.ivars().config.borrow_mut();
            change(&mut config);
            config.normalize_modes();
            config.clone()
        };
        if let Err(e) = config.save(&config_path()) {
            tf_core::log_error!("could not save the config: {e}");
        }
        self.reset_triggers(&config);
        if let Some(core) = self.ivars().core.get() {
            core.reload(config);
        }
        self.refresh_mode_menu();
        self.update_status();
        self.apply_dock_policy();
        self.apply_pill_style();
    }

    fn apply_pill_style(&self) {
        let Some(pill) = self.ivars().pill.get() else {
            return;
        };
        let config = self.ivars().config.borrow();
        pill.set_glow(config.app.processing_glow, &config.app.glow_color);
    }

    pub(crate) fn apply_dock_policy(&self) {
        let policy = if self.ivars().config.borrow().app.show_in_dock {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        NSApplication::sharedApplication(self.mtm()).setActivationPolicy(policy);
    }

    fn reset_triggers(&self, config: &Config) {
        *self.ivars().trigger.borrow_mut() =
            Machine::new(config.trigger.style, config.trigger.hold_threshold_ms);
        self.ivars().mode_machines.borrow_mut().clear();
        let mut triggers: Vec<(tf_core::trigger::TriggerKey, Option<ModeId>)> = config
            .modes()
            .into_iter()
            .filter_map(|mode| mode.hotkey.map(|key| (key, Some(mode.id))))
            .collect();
        triggers.push((config.trigger.key, None));
        if let Ok(mut current) = self.ivars().hotkey_triggers.write() {
            *current = triggers;
        }
        if let Ok(mut current) = self.ivars().mode_switch_key.write() {
            *current = config.trigger.mode_switch_key;
        }
    }

    pub(crate) fn hotkey_paused(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ivars().hotkey_paused)
    }

    pub(crate) fn show_settings(&self, page: Page) {
        let mtm = self.mtm();
        if self.ivars().settings.borrow().is_none() {
            let settings = Settings::new(mtm, self);
            settings.window().setDelegate(Some(ProtocolObject::from_ref(self)));
            *self.ivars().settings.borrow_mut() = Some(settings);
        }
        if let Some(settings) = self.ivars().settings.borrow().as_ref() {
            settings.show(page);
            settings.refresh();
        }
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        if let Some(settings) = self.ivars().settings.borrow().as_ref() {
            settings.window().orderFrontRegardless();
        }
    }

    pub(crate) fn refresh_settings(&self) {
        if let Some(settings) = self.ivars().settings.borrow().as_ref() {
            settings.refresh();
        }
    }

    pub(crate) fn show_history(&self) {
        self.show_settings(Page::History);
        self.reload_history();
        self.refresh_recent();
    }

    pub(crate) fn show_meeting_history(&self) {
        self.show_history();
        self.set_history_mode(true);
    }

    pub(crate) fn stop_meeting(&self) {
        if let Some(core) = self.ivars().core.get() {
            core.stop_meeting();
        }
        *self.ivars().meeting_app.borrow_mut() = None;
        if let Some(live) = self.ivars().live.get() {
            live.stop_enabled(false);
        }
    }

    pub(crate) fn toggle_live_transcript(&self) {
        let Some(live) = self.ivars().live.get() else {
            return;
        };
        if live.is_visible() {
            live.hide();
        } else if self.ivars().meeting_id.get().is_some() {
            let title = self.ivars().meeting_app.borrow().clone().unwrap_or_else(|| "Meeting".into());
            live.show(&title);
        }
        self.update_status();
    }

    fn start_meeting_now(&self) {
        let detected = self.ivars().detected_app.borrow_mut().take();
        if let Some(app) = detected {
            self.start_meeting_with(Some(app));
            return;
        }
        let frontmost = NSWorkspace::sharedWorkspace().frontmostApplication();
        let bundle = frontmost
            .as_ref()
            .and_then(|application| application.bundleIdentifier())
            .map(|id| id.to_string());
        let name = frontmost.and_then(|application| application.localizedName()).map(|name| name.to_string());
        if let Some(core) = self.ivars().core.get() {
            core.start_meeting(bundle, name, Vec::new());
        }
    }

    fn start_detected_meeting(&self) {
        let detected = self.ivars().detected_app.borrow_mut().take();
        self.start_meeting_with(detected);
    }

    fn dismiss_detected_meeting(&self) {
        if let Some(app) = self.ivars().detected_app.borrow_mut().take() {
            self.ivars().detector.borrow_mut().not_now(&app, Instant::now());
        }
    }

    fn start_meeting_with(&self, app: Option<DetectedApp>) {
        let config = self.ivars().config.borrow().clone();
        let (bundle, name, pids) = match app {
            Some(app) => {
                let pids = if config.meetings.capture == tf_core::config::CaptureMode::App {
                    vec![app.pid]
                } else {
                    Vec::new()
                };
                (Some(app.bundle_id), Some(app.name), pids)
            }
            None => (None, None, Vec::new()),
        };
        if let Some(core) = self.ivars().core.get() {
            core.start_meeting(bundle, name, pids);
        }
    }

    fn start_detection_timer(&self) {
        if self.ivars().detect_timer.borrow().is_some() {
            return;
        }
        let this = self.retain();
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| this.poll_detection());
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(3.0, true, &block) };
        *self.ivars().detect_timer.borrow_mut() = Some(timer);
    }

    fn poll_detection(&self) {
        let now = Instant::now();
        let apps = tf_meeting::detect::running_input_apps();
        let meeting_app = self.ivars().meeting_app.borrow().clone();
        if let Some(bundle) = meeting_app {
            if let (Some(started), Some(live)) = (self.ivars().meeting_started.get(), self.ivars().live.get())
                && live.is_visible()
            {
                live.set_elapsed(started.elapsed().as_secs());
            }
            if self.ivars().detector.borrow_mut().should_stop(&apps, &bundle, now) {
                self.stop_meeting();
            }
            self.update_status();
            return;
        }
        if !self.ivars().config.borrow().meetings.detect {
            return;
        }
        if let Some(app) = self.ivars().detector.borrow_mut().observe(&apps, now) {
            *self.ivars().detected_app.borrow_mut() = Some(app.clone());
            self.notify_meeting_detected(&app);
            // einmal pro anruf fragen. die start-aktion der notification geht weiter, während neue prompts für diese app eine stunde unterdrückt bleiben.
            self.ivars().detector.borrow_mut().not_now(&app, now);
        }
    }

    #[allow(deprecated)]
    fn notify_meeting_detected(&self, app: &DetectedApp) {
        let notification = NSUserNotification::new();
        notification.setIdentifier(Some(ns_string!("tf-meeting-start")));
        notification.setTitle(Some(&NSString::from_str("Transcribe this call?")));
        notification.setInformativeText(Some(&NSString::from_str(&format!(
            "{} is using the microphone. tf can record and transcribe it on this Mac.",
            app.name
        ))));
        notification.setActionButtonTitle(ns_string!("Start"));
        notification.setHasActionButton(true);
        let not_now = NSUserNotificationAction::actionWithIdentifier_title(
            Some(ns_string!("tf-meeting-not-now")),
            Some(ns_string!("Not now")),
        );
        notification.setAdditionalActions(Some(&NSArray::from_retained_slice(&[not_now])));
        NSUserNotificationCenter::defaultUserNotificationCenter().deliverNotification(&notification);
    }

    fn set_meeting_ui(&self, active: bool) {
        if let Some(item) = self.ivars().meeting_item.get() {
            item.setTitle(&NSString::from_str(if active {
                "Stop Meeting Recording"
            } else {
                "Start Meeting Recording…"
            }));
            unsafe { item.setAction(Some(if active { sel!(stopMeeting:) } else { sel!(startMeeting:) })) };
        }
        if let Some(item) = self.ivars().status_item.get()
            && let Some(button) = item.button(self.mtm())
        {
            let symbol = if active { "waveform.circle.fill" } else { "waveform" };
            let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                Some(ns_string!("tf")),
            );
            button.setImage(icon.as_deref());
            let tint = active.then(NSColor::systemRedColor);
            button.setContentTintColor(tint.as_deref());
        }
        self.update_status();
    }

    pub(crate) fn with_history<R>(&self, f: impl FnOnce(&History) -> R) -> Option<R> {
        self.ivars().settings.borrow().as_ref().map(|settings| f(settings.history()))
    }

    pub(crate) fn reload_history(&self) {
        self.with_history(History::reload);
    }

    pub(crate) fn load_more_history(&self) {
        self.with_history(History::load_more);
    }

    pub(crate) fn set_history_mode(&self, meetings: bool) {
        self.with_history(|history| history.set_mode(meetings));
    }

    pub(crate) fn regenerate_selected_summary(&self) {
        let Some(id) = self.with_history(History::selected_meeting_id).flatten() else {
            return;
        };
        if let Some(core) = self.ivars().core.get() {
            core.regenerate_summary(id);
        }
    }

    fn selected_meeting(&self) -> Option<tf_store::Meeting> {
        self.with_history(History::selected_meeting).flatten()
    }

    fn meeting_text(&self, meeting: &tf_store::Meeting) -> String {
        let segments = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.segments(meeting.id).ok())
            .unwrap_or_default();
        let speakers = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.meeting_speakers(meeting.id).ok())
            .unwrap_or_default();
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
            .map(|(segment, label)| tf_meeting::summary::Line {
                speaker: label.as_str(),
                start_ms: segment.start_ms,
                text: &segment.text,
            })
            .collect::<Vec<_>>();
        let transcript = tf_meeting::summary::transcript_markdown(lines);
        match meeting.summary.as_deref() {
            Some(summary) => format!("{}\n\n## Transcript\n{transcript}", summary.trim()),
            None => transcript,
        }
    }

    pub(crate) fn copy_selected(&self) {
        if self.with_history(History::is_meetings).unwrap_or(false) {
            let Some(meeting) = self.selected_meeting() else {
                return;
            };
            let text = self.meeting_text(&meeting);
            insert::copy(&text);
            return;
        }
        let Some(take) = self.selected_take() else {
            return;
        };
        insert::copy(&take.final_text);
    }

    pub(crate) fn play_selected(&self) {
        if self.with_history(History::is_meetings).unwrap_or(false) {
            self.play_meeting();
            return;
        }
        let Some(take) = self.selected_take() else {
            return;
        };
        let Some(audio) = take.audio_file else {
            return;
        };
        let path = audio_dir().join(audio);
        self.play_file(&path);
    }

    fn play_meeting(&self) {
        let Some(meeting) = self.selected_meeting() else {
            return;
        };
        let root = support_dir();
        let me = meeting.audio_me.map(|path| tf_audio::file::read(&root.join(path)).unwrap_or_default());
        let them = meeting.audio_them.map(|path| tf_audio::file::read(&root.join(path)).unwrap_or_default());
        let mut samples = me.unwrap_or_default();
        if let Some(them) = them {
            if samples.len() < them.len() {
                samples.resize(them.len(), 0.0);
            }
            for (index, sample) in them.into_iter().enumerate() {
                samples[index] += sample;
            }
        }
        if samples.is_empty() {
            return;
        }
        let path = support_dir().join("meetings").join("playback.m4a");
        if let Err(error) = tf_audio::file::write_m4a(&path, &samples) {
            tf_core::log_error!("could not prepare playback: {error}");
            return;
        }
        self.play_file(&path);
    }

    fn play_file(&self, path: &std::path::Path) {
        self.play_file_range(path, 0.0, f64::INFINITY);
    }

    /// spielt `[begin, end)` in sekunden; `end` unendlich heißt bis zum ende.
    fn play_file_range(&self, path: &std::path::Path, begin: f64, end: f64) {
        self.stop_playback();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        // SAFETY: die url ist gültig, avaudioplayer meldet fehler über den error.
        let player = unsafe { AVAudioPlayer::initWithContentsOfURL_error(AVAudioPlayer::alloc(), &url) };
        match player {
            Ok(player) => {
                // SAFETY: der player wurde gerade erzeugt und läuft nur auf dem main-thread.
                unsafe { player.setCurrentTime(begin) };
                let _ = unsafe { player.play() };
                if end.is_finite() {
                    let stopper = player.clone();
                    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
                        // SAFETY: einen fertigen oder ersetzten player zu stoppen ist ein no-op.
                        unsafe { stopper.stop() };
                    });
                    // SAFETY: der block fasst nur den retained player an.
                    unsafe {
                        NSTimer::scheduledTimerWithTimeInterval_repeats_block(end - begin, false, &block)
                    };
                }
                *self.ivars().player.borrow_mut() = Some(player);
            }
            Err(error) => {
                tf_core::log_error!("could not play {}: {}", path.display(), error.localizedDescription());
            }
        }
    }

    pub(crate) fn rerun_selected(&self, engine: &str) {
        let Some(id) = self.history_selected() else {
            return;
        };
        if let Some(core) = self.ivars().core.get() {
            core.rerun(id, Some(engine.to_owned()));
        }
    }

    fn stop_playback(&self) {
        if let Some(player) = self.ivars().player.borrow_mut().take() {
            // SAFETY: der player wird nur auf dem main-thread benutzt.
            unsafe { player.stop() };
        }
    }

    pub(crate) fn speaker_edit(&self, edit: tf_core::SpeakerEdit) {
        self.stop_playback();
        if let Some(core) = self.ivars().core.get() {
            core.speaker_edit(edit);
        }
    }

    pub(crate) fn rerun_diarization(&self) {
        let Some(id) = self.with_history(History::selected_meeting_id).flatten() else {
            return;
        };
        if let Some(core) = self.ivars().core.get() {
            core.rerun_diarization(id);
        }
    }

    pub(crate) fn copy_text(&self, text: &str) {
        insert::copy(text);
    }

    pub(crate) fn rename_speaker(&self, speaker_id: i64) {
        let current = Store::open_read_only(&history_path()).ok().and_then(|store| {
            let id = self.with_history(History::selected_meeting_id).flatten()?;
            store
                .meeting_speakers(id)
                .ok()
                .and_then(|speakers| speakers.into_iter().find(|speaker| speaker.id == speaker_id))
        });
        let Some(current) = current else {
            return;
        };
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str("Rename speaker"));
        alert.setInformativeText(&NSString::from_str(
            "Leave the field empty to go back to the speaker number.",
        ));
        let field = objc2_app_kit::NSTextField::textFieldWithString(
            &NSString::from_str(current.name.as_deref().unwrap_or("")),
            self.mtm(),
        );
        field.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 24.0)));
        field.setPlaceholderString(Some(&NSString::from_str(&current.label())));
        alert.setAccessoryView(Some(&field));
        alert.window().setInitialFirstResponder(Some(&field));
        alert.addButtonWithTitle(&NSString::from_str("Save"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            let name = field.stringValue().to_string();
            self.speaker_edit(tf_core::SpeakerEdit::Rename {
                speaker_id,
                name: (!name.trim().is_empty()).then_some(name),
            });
        }
    }

    pub(crate) fn play_meeting_range(&self, meeting_id: MeetingId, start_ms: u64, end_ms: u64) {
        let Some(meeting) = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.meeting(meeting_id).ok().flatten())
        else {
            return;
        };
        let Some(relative) = meeting.audio_them else {
            return;
        };
        let offset = meeting.them_offset_ms.unwrap_or(0);
        let begin = start_ms.saturating_sub(offset) as f64 / 1_000.0;
        let end = end_ms.saturating_sub(offset) as f64 / 1_000.0;
        if end <= begin {
            return;
        }
        self.play_file_range(&support_dir().join(relative), begin, end);
    }

    pub(crate) fn play_speaker_sample(&self, meeting_id: MeetingId, speaker_id: i64) {
        let segments = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.segments(meeting_id).ok())
            .unwrap_or_default();
        let best = segments
            .iter()
            .filter(|segment| {
                segment.speaker == tf_store::Speaker::Them && segment.speaker_id == Some(speaker_id)
            })
            .max_by_key(|segment| (!segment.overlap, segment.end_ms - segment.start_ms));
        if let Some(segment) = best {
            let end = segment.end_ms.min(segment.start_ms + SAMPLE_MAX_MS);
            self.play_meeting_range(meeting_id, segment.start_ms, end);
        }
    }

    pub(crate) fn split_speaker(&self, speaker_id: i64) {
        let Some(meeting_id) = self.with_history(History::selected_meeting_id).flatten() else {
            return;
        };
        let Ok(store) = Store::open_read_only(&history_path()) else {
            return;
        };
        let Ok(rows) = store.segment_embeddings(meeting_id) else {
            return;
        };
        let (ids, vectors): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .filter(|(_, speaker, locked, _)| *speaker == Some(speaker_id) && !*locked)
            .filter_map(|(id, _, _, embedding)| embedding.map(|embedding| (id, embedding)))
            .unzip();
        if vectors.len() < 2 {
            return;
        }
        let Some(split) = tf_diarize::split_two(&vectors) else {
            return;
        };
        let groups = split.map(|group| group.into_iter().map(|index| ids[index]).collect()).to_vec();
        self.speaker_edit(tf_core::SpeakerEdit::Split { speaker_id, groups });
    }

    pub(crate) fn delete_selected(&self) {
        if self.with_history(History::is_meetings).unwrap_or(false) {
            let ids = self.with_history(History::selected_meeting_ids).unwrap_or_default();
            if ids.is_empty() {
                return;
            }
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&NSString::from_str(if ids.len() == 1 {
                "Delete this meeting?"
            } else {
                "Delete these meetings?"
            }));
            alert.setInformativeText(&NSString::from_str(
                "The transcript, summary and stored audio are removed permanently.",
            ));
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.addButtonWithTitle(&NSString::from_str("Delete"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            if alert.runModal() == NSAlertFirstButtonReturn
                && let Some(core) = self.ivars().core.get()
            {
                core.delete_meetings(ids);
            }
            return;
        }
        let ids = self.history_selected_ids();
        if ids.is_empty() {
            return;
        }
        let alert = NSAlert::new(self.mtm());
        if ids.len() == 1 {
            alert.setMessageText(&NSString::from_str("Delete this dictation?"));
        } else {
            alert.setMessageText(&NSString::from_str(&format!("Delete {} dictations?", ids.len())));
        }
        alert.setInformativeText(&NSString::from_str(
            "The transcripts are removed permanently. Stored audio is deleted as well.",
        ));
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.addButtonWithTitle(&NSString::from_str("Delete"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() == NSAlertFirstButtonReturn
            && let Some(core) = self.ivars().core.get()
        {
            core.delete_takes(ids);
        }
    }

    pub(crate) fn delete_all_history(&self) {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str("Delete all history?"));
        alert.setInformativeText(&NSString::from_str(
            "Every transcript and every stored audio file is removed. This cannot be undone.",
        ));
        alert.setAlertStyle(NSAlertStyle::Critical);
        alert.addButtonWithTitle(&NSString::from_str("Delete All"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() == NSAlertFirstButtonReturn
            && let Some(core) = self.ivars().core.get()
        {
            core.delete_all_history();
        }
    }

    /// löscht den verlauf ohne weitere rückfrage; aufrufer haben schon gefragt.
    pub(crate) fn clear_history_now(&self) {
        if let Some(core) = self.ivars().core.get() {
            core.delete_all_history();
        }
    }

    pub(crate) fn clear_audio_now(&self) {
        if let Some(core) = self.ivars().core.get() {
            core.delete_all_audio();
        }
    }

    pub(crate) fn history_visible_rows(&self) -> Vec<tf_store::TakeSummary> {
        self.with_history(History::visible_rows).unwrap_or_default()
    }

    #[allow(deprecated)]
    pub(crate) fn export_history(&self) {
        if self.with_history(History::is_meetings).unwrap_or(false) {
            self.export_meetings();
            return;
        }
        let mut rows = self.history_visible_rows();
        if rows.is_empty() {
            return;
        }
        let selected = self.history_selected_ids();
        if !selected.is_empty() {
            rows.retain(|row| selected.contains(&row.id));
        }
        if rows.is_empty() {
            return;
        }
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(&NSString::from_str("tf-history.md"));
        panel.setAllowedFileTypes(Some(&objc2_foundation::NSArray::from_retained_slice(&[
            NSString::from_str("md"),
            NSString::from_str("json"),
        ])));
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let Some(url) = panel.URL() else {
            return;
        };
        let Some(path) = url.path().map(|path| path.to_string()) else {
            return;
        };
        let json = path.ends_with(".json");
        let text = if json {
            let entries: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id,
                        "created_at": row.created_at,
                        "duration_ms": row.duration_ms,
                        "app": row.app_name,
                        "bundle_id": row.app_bundle_id,
                        "mode": row.mode_name,
                        "text": row.final_text,
                        "has_audio": row.has_audio,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into())
        } else {
            let mut out = String::new();
            for row in &rows {
                let app = row.app_name.clone().or(row.app_bundle_id.clone()).unwrap_or_else(|| "tf".into());
                let date = NSDateFormatter::new();
                date.setDateFormat(Some(&NSString::from_str("yyyy-MM-dd HH:mm")));
                let when = date
                    .stringFromDate(&objc2_foundation::NSDate::dateWithTimeIntervalSince1970(
                        row.created_at as f64 / 1000.0,
                    ))
                    .to_string();
                out.push_str(&format!("## {when} — {app}\n\n{}\n\n", row.final_text));
            }
            out
        };
        if let Err(e) = std::fs::write(&path, text) {
            tf_core::log_error!("could not export history: {e}");
        }
    }

    #[allow(deprecated)]
    fn export_meetings(&self) {
        let mut rows = self.with_history(History::visible_meetings).unwrap_or_default();
        if rows.is_empty() {
            return;
        }
        let selected = self.with_history(History::selected_meeting_ids).unwrap_or_default();
        if !selected.is_empty() {
            rows.retain(|row| selected.contains(&row.id));
        }
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(&NSString::from_str("tf-meetings.md"));
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[
            NSString::from_str("md"),
            NSString::from_str("json"),
        ])));
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let Some(url) = panel.URL() else {
            return;
        };
        let Some(path) = url.path().map(|path| path.to_string()) else {
            return;
        };
        let store = Store::open_read_only(&history_path()).ok();
        let mut rendered = Vec::new();
        for row in &rows {
            let Some(meeting) = store.as_ref().and_then(|store| store.meeting(row.id).ok().flatten()) else {
                continue;
            };
            let transcript = self.meeting_text(&meeting);
            rendered.push((row, meeting, transcript));
        }
        let text = if path.ends_with(".json") {
            let entries: Vec<serde_json::Value> = rendered
                .iter()
                .map(|(row, meeting, transcript)| {
                    serde_json::json!({
                        "id": row.id,
                        "title": meeting.title,
                        "started_at": meeting.started_at,
                        "duration_ms": row.duration_ms,
                        "app": meeting.app_name,
                        "bundle_id": meeting.app_bundle_id,
                        "summary": meeting.summary,
                        "transcript": transcript,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into())
        } else {
            let mut out = String::new();
            for (row, meeting, transcript) in &rendered {
                let date = NSDateFormatter::new();
                date.setDateFormat(Some(&NSString::from_str("yyyy-MM-dd HH:mm")));
                let when = date
                    .stringFromDate(&objc2_foundation::NSDate::dateWithTimeIntervalSince1970(
                        row.started_at as f64 / 1000.0,
                    ))
                    .to_string();
                out.push_str(&format!("# {} — {when}\n\n{}", meeting.title, transcript));
                out.push_str("\n\n---\n\n");
            }
            out
        };
        if let Err(error) = std::fs::write(&path, text) {
            tf_core::log_error!("could not export meetings: {error}");
        }
    }

    pub(crate) fn insert_text(&self, text: &str) {
        let (method, append_space, extras) = {
            let config = self.ivars().config.borrow();
            (config.insertion.method, config.insertion.append_space, insert_extras(&config))
        };
        match insert::insert(&shape(text, append_space), method, extras) {
            Ok(Outcome::Inserted) => {}
            Ok(Outcome::CopiedSecure | Outcome::CopiedNoAccess) => {}
            Err(e) => tf_core::log_error!("insertion failed: {e}"),
        }
    }

    fn selected_take(&self) -> Option<tf_store::Take> {
        let id = self.history_selected()?;
        Store::open_read_only(&history_path()).ok()?.get(id).ok().flatten()
    }

    fn history_selected(&self) -> Option<TakeId> {
        self.with_history(History::selected_id).flatten()
    }

    fn history_selected_ids(&self) -> Vec<TakeId> {
        self.with_history(History::selected_ids).unwrap_or_default()
    }

    fn refresh_mode_menu(&self) {
        let Some(menu) = self.ivars().mode_menu.get() else {
            return;
        };
        menu.removeAllItems();
        let config = self.ivars().config.borrow().clone();
        let modes = config.modes();
        *self.ivars().mode_list.borrow_mut() = modes.iter().map(|mode| mode.id.clone()).collect();
        for (index, mode) in modes.iter().enumerate() {
            let title = match mode.hotkey {
                Some(key) => format!("{}   {}", mode.name, key.display()),
                None => mode.name.clone(),
            };
            let item = menu_item(self.mtm(), &title, Some(sel!(selectMode:)), "");
            unsafe { item.setTarget(Some(self)) };
            item.setTag(index as isize);
            item.setState(if mode.id == config.default_mode_id {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            menu.addItem(&item);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        let edit = menu_item(self.mtm(), "Edit Modes…", Some(sel!(editModes:)), "");
        unsafe { edit.setTarget(Some(self)) };
        menu.addItem(&edit);
    }

    fn refresh_recent(&self) {
        let Some(menu) = self.ivars().recent_menu.get() else {
            return;
        };
        menu.removeAllItems();
        let rows = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.list(&Query { limit: 5, ..Query::default() }).ok())
            .unwrap_or_default();
        if rows.is_empty() {
            let item = menu_item(self.mtm(), "No dictations yet", None, "");
            item.setEnabled(false);
            menu.addItem(&item);
            return;
        }
        for row in rows {
            let title = first_line(&row.final_text);
            let item = menu_item(self.mtm(), &title, Some(sel!(copyRecent:)), "");
            unsafe { item.setTarget(Some(self)) };
            item.setTag(row.id as isize);
            menu.addItem(&item);
        }
    }

    pub(crate) fn download_view(&self) -> Option<DownloadView> {
        self.ivars().download_state.borrow().clone()
    }

    pub(crate) fn download_active(&self) -> bool {
        self.ivars().model_download.borrow().is_some() || self.ivars().speech_install.borrow().is_some()
    }

    pub(crate) fn start_model_download(&self, id: &str) {
        if self.ivars().model_download.borrow().is_some() || self.ivars().speech_install.borrow().is_some() {
            return;
        }
        let Ok(catalog) = Catalog::embedded() else {
            return;
        };
        let Some(entry) = catalog.find(id).cloned() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let download = tf_models::install(&entry, &support_dir(), move |progress| {
            let _ = tx.send(progress);
        });
        *self.ivars().model_download.borrow_mut() = Some((download, rx));
        *self.ivars().download_state.borrow_mut() = Some(DownloadView {
            id: entry.id.clone(),
            progress: 0.0,
            total: entry.total_bytes(),
            phase: DownloadPhase::Downloading,
        });
        self.start_download_timer();
        self.refresh_settings();
    }

    pub(crate) fn start_speech_install(&self, locale: &str) {
        if self.ivars().model_download.borrow().is_some() || self.ivars().speech_install.borrow().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let locale_owned = locale.to_owned();
        let progress_tx = tx.clone();
        let spawned = std::thread::Builder::new().name("tf-speech-install".into()).spawn(move || {
            let result = tf_apple::speech::install(&locale_owned, move |value| {
                let _ = progress_tx.send(SpeechInstallMsg::Progress(value));
            });
            let _ = tx.send(SpeechInstallMsg::Done(result.map_err(|e| e.to_string())));
        });
        if spawned.is_err() {
            return;
        }
        *self.ivars().speech_install.borrow_mut() = Some(rx);
        *self.ivars().download_state.borrow_mut() = Some(DownloadView {
            id: format!("apple-speech:{locale}"),
            progress: 0.0,
            total: 0,
            phase: DownloadPhase::Downloading,
        });
        self.start_download_timer();
        self.refresh_settings();
    }

    pub(crate) fn cancel_download(&self) {
        if let Some((download, _)) = self.ivars().model_download.borrow().as_ref() {
            download.cancel();
        }
        if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
            state.phase = DownloadPhase::Cancelled;
        }
        self.refresh_settings();
    }

    pub(crate) fn set_default_engine(&self, id: &str) {
        if self.config().engine.default == id {
            return;
        }
        self.ivars().engine_ready.set(false);
        self.ivars().engine_failure.replace(None);
        self.update_config(|config| config.engine.default = id.to_owned());
        self.update_status();
        self.refresh_settings();
    }

    pub(crate) fn delete_model(&self, id: &str) {
        let catalog = Catalog::embedded().ok();
        let models = models_dir();
        let is_runtime =
            catalog.as_ref().and_then(|catalog| catalog.find(id)).is_some_and(|entry| entry.is_runtime());
        let root = if is_runtime { runtime_root(&support_dir()) } else { models.clone() };
        if let Err(e) = tf_models::uninstall(&root, id) {
            tf_core::log_error!("could not delete {id}: {e}");
            return;
        }
        if is_runtime {
            self.refresh_settings();
            return;
        }
        if self.config().engine.default == id {
            let installed = tf_models::scan(&models);
            let next = installed
                .iter()
                .find(|model| catalog.as_ref().map(|c| c.find(&model.id).is_some()).unwrap_or(false))
                .map(|model| model.id.clone());
            if let Some(next) = next {
                self.set_default_engine(&next);
            } else {
                self.ivars().engine_ready.set(false);
                self.ivars().engine_failure.replace(Some(Failure::ModelMissing(models.join(id))));
                if let Some(core) = self.ivars().core.get() {
                    core.warm();
                }
                self.update_status();
            }
        }
        self.refresh_settings();
    }

    pub(crate) fn test_endpoint(&self) {
        let Some(endpoint) = self.config().llm.endpoints.first().cloned() else {
            return;
        };
        let dispatcher = MainDispatcher::new(self);
        std::thread::Builder::new()
            .name("tf-endpoint-test".into())
            .spawn(move || {
                let message = match tf_llm::list_models(&endpoint.base_url, None) {
                    Ok(models) if models.is_empty() => {
                        format!("{} responded but lists no models.", endpoint.name)
                    }
                    Ok(models) => format!("{}: {}", endpoint.name, models.join(", ")),
                    Err(e) => format!("{} failed: {e}", endpoint.name),
                };
                dispatcher.send(Message::Core(Event::Notice(message)));
            })
            .ok();
    }

    fn start_download_timer(&self) {
        if self.ivars().download_timer.borrow().is_some() {
            return;
        }
        let this = self.retain();
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| this.poll_downloads());
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &block) };
        *self.ivars().download_timer.borrow_mut() = Some(timer);
    }

    fn stop_download_timer(&self) {
        if let Some(timer) = self.ivars().download_timer.take() {
            timer.invalidate();
        }
    }

    fn poll_downloads(&self) {
        let messages: Vec<ModelProgress> = match self.ivars().model_download.borrow().as_ref() {
            Some((_, rx)) => {
                let mut out = Vec::new();
                while let Ok(progress) = rx.try_recv() {
                    out.push(progress);
                }
                out
            }
            None => Vec::new(),
        };
        for message in messages {
            self.model_progress(message);
        }

        let speech: Vec<SpeechInstallMsg> = match self.ivars().speech_install.borrow().as_ref() {
            Some(rx) => {
                let mut out = Vec::new();
                while let Ok(message) = rx.try_recv() {
                    out.push(message);
                }
                out
            }
            None => Vec::new(),
        };
        for message in speech {
            self.speech_progress(message);
        }

        if self.ivars().model_download.borrow().is_none()
            && self.ivars().speech_install.borrow().is_none()
            && !matches!(
                self.ivars().download_state.borrow().as_ref().map(|s| &s.phase),
                Some(DownloadPhase::Optimizing)
            )
        {
            self.stop_download_timer();
        }
        self.refresh_settings();
    }

    fn model_progress(&self, progress: ModelProgress) {
        let mut finished = false;
        match progress {
            ModelProgress::Started { total } => {
                if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                    state.total = total;
                    state.progress = 0.0;
                }
            }
            ModelProgress::Advanced { done } => {
                if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                    state.progress = if state.total > 0 {
                        (done as f64 / state.total as f64).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                }
            }
            ModelProgress::Verifying => {
                if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                    state.phase = DownloadPhase::Verifying;
                }
            }
            ModelProgress::Installed => {
                let id = self
                    .ivars()
                    .download_state
                    .borrow()
                    .as_ref()
                    .map(|state| state.id.clone())
                    .unwrap_or_default();
                let kind = Catalog::embedded()
                    .ok()
                    .and_then(|catalog| catalog.find(&id).map(|entry| entry.kind.clone()))
                    .unwrap_or_default();
                if kind == "diarization" {
                    let models = models_dir();
                    std::thread::Builder::new()
                        .name("tf-diarize-compile".into())
                        .spawn(move || {
                            // core ml graphs schon jetzt kompilieren, nicht erst beim ersten aufruf.
                            let _ = tf_diarize::Diarizer::load(&models);
                        })
                        .ok();
                    if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                        state.progress = 1.0;
                        state.phase = DownloadPhase::Installed;
                    }
                } else if kind != "stt" {
                    if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                        state.progress = 1.0;
                        state.phase = DownloadPhase::Installed;
                    }
                } else {
                    let default_missing = !self.model_installed(&self.config().engine.default);
                    if default_missing {
                        self.set_default_engine(&id);
                    }
                    if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                        state.progress = 1.0;
                        state.phase = DownloadPhase::Optimizing;
                    }
                    if self.config().engine.default == id
                        && let Some(core) = self.ivars().core.get()
                    {
                        if !default_missing {
                            core.warm();
                        }
                    } else if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                        state.phase = DownloadPhase::Installed;
                    }
                }
                finished = true;
            }
            ModelProgress::Failed(message) => {
                if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                    state.phase = DownloadPhase::Failed(message);
                }
                finished = true;
            }
            ModelProgress::Cancelled => {
                if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                    state.phase = DownloadPhase::Cancelled;
                }
                finished = true;
            }
        }
        if finished {
            self.ivars().model_download.borrow_mut().take();
            let installed_id = self
                .ivars()
                .download_state
                .borrow()
                .as_ref()
                .filter(|state| {
                    state.phase == DownloadPhase::Installed
                        || matches!(state.phase, DownloadPhase::Optimizing)
                })
                .map(|state| state.id.clone());
            if let Some(id) = installed_id
                && let Ok(catalog) = Catalog::embedded()
                && catalog.find(&id).is_some_and(|entry| entry.kind == "llm")
                && tf_models::scan_runtimes(&support_dir()).is_empty()
            {
                self.start_model_download("llama.cpp-b11669");
            }
        }
    }

    fn speech_progress(&self, message: SpeechInstallMsg) {
        match message {
            SpeechInstallMsg::Progress(value) => {
                if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                    state.progress = value.clamp(0.0, 1.0);
                }
            }
            SpeechInstallMsg::Done(result) => {
                match result {
                    Ok(()) => {
                        let id = self
                            .ivars()
                            .download_state
                            .borrow()
                            .as_ref()
                            .map(|state| state.id.clone())
                            .unwrap_or_default();
                        let current = self.config().engine.default;
                        if !self.model_installed(&current) || current.starts_with("apple-speech:") {
                            self.set_default_engine(&id);
                        }
                        if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                            state.progress = 1.0;
                            state.phase = DownloadPhase::Installed;
                        }
                    }
                    Err(e) => {
                        if let Some(state) = self.ivars().download_state.borrow_mut().as_mut() {
                            state.phase = DownloadPhase::Failed(e);
                        }
                    }
                }
                self.ivars().speech_install.borrow_mut().take();
            }
        }
    }

    fn finish_optimizing(&self) {
        if let Some(state) = self.ivars().download_state.borrow_mut().as_mut()
            && state.phase == DownloadPhase::Optimizing
        {
            state.phase = DownloadPhase::Installed;
        }
    }

    fn download_failed(&self, failure: &Failure) {
        if let Some(state) = self.ivars().download_state.borrow_mut().as_mut()
            && state.phase == DownloadPhase::Optimizing
        {
            state.phase = DownloadPhase::Failed(failure.message());
        }
        self.stop_download_timer();
    }

    fn launch(&self) {
        let mtm = self.mtm();
        let path = config_path();
        let first_launch = !path.exists();
        let loaded = Config::load_or_backup(&path);
        if let Some(warning) = loaded.warning {
            tf_core::log_warn!("{warning}");
        }
        let config = loaded.config;
        if first_launch && let Err(e) = config.save(&path) {
            tf_core::log_warn!("could not write the default config: {e}");
        }
        tf_models::cleanup_downloads(&support_dir().join("downloads"), Duration::from_secs(7 * 24 * 60 * 60));
        tf_llm::cleanup_local(&support_dir());
        adopt_legacy_models();
        *self.ivars().trigger.borrow_mut() =
            Machine::new(config.trigger.style, config.trigger.hold_threshold_ms);
        self.reset_triggers(&config);
        *self.ivars().config.borrow_mut() = config.clone();

        self.ivars().status_item.get_or_init(|| self.build_status_item(mtm));
        self.ivars().pill.get_or_init(|| Pill::new(mtm));
        self.apply_pill_style();
        self.ivars().live.get_or_init(|| LiveTranscript::new(mtm, self));
        #[allow(deprecated)]
        unsafe {
            NSUserNotificationCenter::defaultUserNotificationCenter()
                .setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        self.refresh_recent();

        let level = Arc::clone(&self.ivars().level);
        let dispatcher = MainDispatcher::new(self);
        let core = Core::spawn(config.clone(), models_dir(), move |event| match event {
            Event::Level(level_value) => {
                level.store(level_value.to_bits(), Ordering::Relaxed);
            }
            other => dispatcher.send(Message::Core(other)),
        });
        let _ = self.ivars().core.set(core);

        if let Err(e) = hotkey::spawn(
            Arc::clone(&self.ivars().hotkey_triggers),
            Arc::clone(&self.ivars().mode_switch_key),
            Arc::clone(&self.ivars().hotkey_paused),
            dispatcher,
            Arc::clone(&self.ivars().intercept_esc),
        ) {
            tf_core::log_error!("hotkey: {e}");
            self.ivars().tap_failed.set(true);
        }
        self.update_status();
        self.start_detection_timer();

        if first_launch || !permissions_ok() {
            let this = self.retain();
            let block = RcBlock::new(move |_timer: NonNull<NSTimer>| this.show_setup());
            unsafe {
                NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.4, false, &block);
            }
        }

        if let Some(name) = std::env::var_os("TF_OPEN_SETTINGS") {
            let page = Page::from_name(&name.to_string_lossy());
            let this = self.retain();
            let block = RcBlock::new(move |_timer: NonNull<NSTimer>| this.show_settings(page));
            unsafe {
                NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.6, false, &block);
            }
        }
    }

    fn handle(&self, message: Message) {
        match message {
            Message::Core(event) => self.core_event(event),
            Message::Hotkey { trigger, event } => self.hotkey(trigger, event),
            Message::HotkeyModeSwitch { back } => self.cycle_mode(back),
            Message::HotkeyError(error) => {
                tf_core::log_error!("{error}");
                self.ivars().tap_failed.set(true);
                self.update_status();
                self.show_settings(Page::Permissions);
            }
        }
    }

    fn hotkey(&self, trigger: Option<ModeId>, event: HotkeyEvent) {
        let now = self.ivars().clock.get().elapsed();
        let input = match event {
            HotkeyEvent::Down => Input::Down(now),
            HotkeyEvent::Up => Input::Up(now),
            HotkeyEvent::OtherKey => Input::OtherKey(now),
            HotkeyEvent::Esc => Input::Esc,
        };
        let config = self.ivars().config.borrow().clone();
        let action = if matches!(event, HotkeyEvent::OtherKey | HotkeyEvent::Esc) {
            let Some(active) = self.ivars().active_trigger.borrow().clone() else {
                return;
            };
            let mut machines = self.ivars().mode_machines.borrow_mut();
            let machine = machines
                .entry(active)
                .or_insert_with(|| Machine::new(config.trigger.style, config.trigger.hold_threshold_ms));
            machine.handle(input)
        } else {
            if let Some(active) = self.ivars().active_trigger.borrow().clone()
                && active != trigger
            {
                return;
            }
            let mut machines = self.ivars().mode_machines.borrow_mut();
            let machine = machines
                .entry(trigger.clone())
                .or_insert_with(|| Machine::new(config.trigger.style, config.trigger.hold_threshold_ms));
            machine.handle(input)
        };
        match action {
            Some(Action::Start) => {
                *self.ivars().active_trigger.borrow_mut() = Some(trigger.clone());
                self.start_dictation(trigger);
            }
            Some(Action::Stop) => {
                *self.ivars().active_trigger.borrow_mut() = None;
                self.stop_dictation();
            }
            Some(Action::Cancel) => {
                *self.ivars().active_trigger.borrow_mut() = None;
                self.cancel_dictation();
            }
            None => {}
        }
    }

    fn cycle_mode(&self, back: bool) {
        let current = self.ivars().current_mode.borrow().clone();
        let next = self.ivars().config.borrow().cycle_mode(&current, !back);
        if let Some(core) = self.ivars().core.get() {
            core.set_mode(next.id);
        }
    }

    fn start_dictation(&self, mode: Option<ModeId>) {
        let frontmost = NSWorkspace::sharedWorkspace().frontmostApplication();
        let app = frontmost
            .as_ref()
            .and_then(|application| application.bundleIdentifier())
            .map(|id| id.to_string());
        let app_name =
            frontmost.and_then(|application| application.localizedName()).map(|name| name.to_string());
        self.ivars().intercept_esc.store(true, Ordering::Relaxed);
        if let Some(core) = self.ivars().core.get() {
            core.start(app, app_name, mode);
        }
    }

    fn stop_dictation(&self) {
        if let Some(core) = self.ivars().core.get() {
            core.stop();
        }
    }

    fn cancel_dictation(&self) {
        if let Some(core) = self.ivars().core.get() {
            core.cancel();
        }
    }

    fn core_event(&self, event: Event) {
        match event {
            Event::EngineLoading => {
                self.update_status();
                self.refresh_settings();
            }
            Event::EngineReady => {
                self.ivars().engine_ready.set(true);
                self.finish_optimizing();
                self.update_status();
                self.refresh_settings();
            }
            Event::EngineFailed(failure) => {
                tf_core::log_error!("{}", failure.message());
                self.download_failed(&failure);
                self.ivars().engine_failure.replace(Some(failure.clone()));
                self.update_status();
                self.refresh_settings();
                if failure.is_model_missing() {
                    self.show_settings(Page::Models);
                }
            }
            Event::Recording { mode } => {
                *self.ivars().current_mode.borrow_mut() = mode.clone();
                self.show_mode_label(&mode, false);
                self.begin_recording();
            }
            Event::ModeChanged { mode } => {
                *self.ivars().current_mode.borrow_mut() = mode.clone();
                self.show_mode_label(&mode, true);
            }
            Event::Level(_) => {}
            Event::Processing => self.begin_processing(),
            Event::Ready(take) => self.finish_take(take),
            Event::Cancelled { empty } => self.cancel_take(empty),
            Event::Failed(failure) => self.fail(failure),
            Event::Notice(_) => {}
            Event::HistoryChanged => {
                self.reload_history();
                self.refresh_recent();
                self.refresh_settings();
            }
            Event::RerunDone(id) => {
                self.reload_history();
                self.with_history(|history| history.select(id));
                self.refresh_settings();
            }
            Event::MeetingStarted { id, title, app_name, app_bundle_id } => {
                self.ivars().meeting_id.set(Some(id));
                *self.ivars().meeting_app.borrow_mut() = app_bundle_id;
                self.ivars().detector.borrow_mut().reset_quiet();
                self.ivars().meeting_started.set(Some(Instant::now()));
                *self.ivars().detected_app.borrow_mut() = None;
                if let Some(live) = self.ivars().live.get() {
                    live.clear();
                    live.stop_enabled(true);
                    live.show(app_name.as_deref().unwrap_or(&title));
                }
                self.set_meeting_ui(true);
                self.reload_history();
            }
            Event::MeetingSegment { meeting_id, speaker, start_ms, text, .. } => {
                if self.ivars().meeting_id.get() == Some(meeting_id)
                    && let Some(live) = self.ivars().live.get()
                {
                    live.append(speaker, start_ms, &text);
                }
                self.with_history(|history| history.meeting_segment_added(meeting_id));
            }
            Event::MeetingFinished { id } => {
                if self.ivars().meeting_id.get() == Some(id) {
                    if let Some(live) = self.ivars().live.get() {
                        live.stop_enabled(false);
                    }
                }
                self.ivars().meeting_started.set(None);
                self.ivars().meeting_id.set(None);
                self.set_meeting_ui(false);
                self.reload_history();
            }
            Event::MeetingSummaryReady { id, .. } => {
                self.reload_history();
                self.with_history(|history| history.select_meeting(id));
            }
            Event::MeetingFailed(message) => {
                tf_core::log_error!("meeting capture failed: {message}");
                self.ivars().meeting_id.set(None);
                *self.ivars().meeting_app.borrow_mut() = None;
                self.ivars().meeting_started.set(None);
                if let Some(live) = self.ivars().live.get() {
                    live.stop_enabled(false);
                    live.hide();
                }
                self.set_meeting_ui(false);
            }
            Event::DiarizationStarted { id } => {
                self.with_history(|history| history.set_diarization_status(id, Some(0.0)));
            }
            Event::DiarizationProgress { id, fraction } => {
                self.with_history(|history| history.set_diarization_status(id, Some(fraction)));
            }
            Event::DiarizationFinished { id } => {
                self.with_history(|history| history.set_diarization_status(id, None));
            }
            Event::DiarizationFailed { id, message } => {
                tf_core::log_error!("speaker identification failed: {message}");
                self.with_history(|history| history.set_diarization_status(id, None));
                self.reload_history();
            }
        }
    }

    fn begin_recording(&self) {
        self.ivars().pill_shown.set(false);
        self.ivars().anim_start.set(Instant::now());
        self.ivars().show_at.set(Some(Instant::now() + PILL_DELAY));
        *self.ivars().phase.borrow_mut() = Phase::Recording;
        self.set_toggle_title("Stop Dictation");
        self.update_status();
        self.start_timer();
    }

    fn show_mode_label(&self, mode_id: &str, force: bool) {
        let config = self.ivars().config.borrow();
        if !force && mode_id == config.default_mode_id {
            return;
        }
        let name = config.mode(mode_id).map(|mode| mode.name).unwrap_or_else(|| mode_id.to_owned());
        drop(config);
        self.pill().show_mode(&name);
        self.ivars().mode_label_until.set(Some(Instant::now() + Duration::from_millis(1500)));
    }

    fn begin_processing(&self) {
        let was_shown = self.ivars().pill_shown.get();
        *self.ivars().phase.borrow_mut() = Phase::Processing;
        if !was_shown {
            self.pill().show(self.mtm());
            self.pill().set_alpha(1.0);
            self.ivars().pill_shown.set(true);
        }
        self.pill().render(Animation::Processing, self.elapsed());
        self.play(Sound::Stop);
        self.set_toggle_title("Start Dictation");
        self.set_status("Transcribing…");
        self.start_timer();
    }

    fn finish_take(&self, take: Take) {
        *self.ivars().active_trigger.borrow_mut() = None;
        let outcome = self.insert_take(&take);
        if let Some(core) = self.ivars().core.get() {
            core.insert_done(outcome);
        }
        self.ivars().last_take.replace(Some(take));
        self.ivars().intercept_esc.store(false, Ordering::Relaxed);
        self.set_toggle_title("Start Dictation");
        if self.ivars().pill_shown.get() {
            *self.ivars().phase.borrow_mut() = Phase::Fading { since: Instant::now() };
        } else {
            *self.ivars().phase.borrow_mut() = Phase::Idle;
            self.stop_timer();
        }
        self.update_status();
    }

    fn cancel_take(&self, empty: bool) {
        *self.ivars().active_trigger.borrow_mut() = None;
        self.ivars().intercept_esc.store(false, Ordering::Relaxed);
        self.set_toggle_title("Start Dictation");
        if empty {
            if !self.ivars().pill_shown.get() {
                self.pill().show(self.mtm());
                self.ivars().pill_shown.set(true);
            }
            self.pill().set_alpha(1.0);
            *self.ivars().phase.borrow_mut() = Phase::Empty { since: Instant::now() };
            self.start_timer();
        } else if self.ivars().pill_shown.get() {
            *self.ivars().phase.borrow_mut() = Phase::Fading { since: Instant::now() };
        } else {
            *self.ivars().phase.borrow_mut() = Phase::Idle;
            self.stop_timer();
        }
        self.update_status();
    }

    fn fail(&self, failure: Failure) {
        tf_core::log_error!("{}", failure.message());
        *self.ivars().active_trigger.borrow_mut() = None;
        self.ivars().trigger.borrow_mut().reset();
        for machine in self.ivars().mode_machines.borrow_mut().values_mut() {
            machine.reset();
        }
        self.ivars().intercept_esc.store(false, Ordering::Relaxed);
        self.pill().show(self.mtm());
        self.pill().set_alpha(1.0);
        self.ivars().pill_shown.set(true);
        *self.ivars().phase.borrow_mut() = Phase::Error { since: Instant::now() };
        self.play(Sound::Error);
        self.set_toggle_title("Start Dictation");
        self.set_status(&failure.message());
        self.start_timer();
        if failure.is_model_missing() {
            self.show_settings(Page::Models);
        } else if matches!(failure, Failure::Audio(_)) {
            self.show_settings(Page::Permissions);
        }
    }

    fn tick(&self) {
        let now = Instant::now();
        let t = self.elapsed();
        let mut next = None;

        if let Some(until) = self.ivars().mode_label_until.get()
            && now >= until
        {
            self.ivars().mode_label_until.set(None);
            self.pill().clear_mode();
        }
        self.pill().step();

        match &*self.ivars().phase.borrow() {
            Phase::Idle => {}
            Phase::Recording => {
                if !self.ivars().pill_shown.get()
                    && let Some(show_at) = self.ivars().show_at.get()
                    && now >= show_at
                {
                    self.ivars().pill_shown.set(true);
                    self.pill().show(self.mtm());
                    self.pill().set_alpha(1.0);
                    self.play(Sound::Start);
                    self.set_status("Listening");
                }
                if self.ivars().pill_shown.get() {
                    let level = f32::from_bits(self.ivars().level.load(Ordering::Relaxed));
                    self.pill().render(Animation::Listening { level }, t);
                }
            }
            Phase::Processing => self.pill().render(Animation::Processing, t),
            Phase::Error { since } => {
                if since.elapsed() >= ERROR_FLASH {
                    next = Some(Phase::Fading { since: now });
                } else {
                    self.pill().render(Animation::Error, t);
                }
            }
            Phase::Empty { since } => {
                let elapsed = since.elapsed();
                if elapsed >= EMPTY_SHAKE {
                    self.pill().shake(0.0);
                    next = Some(Phase::Fading { since: now });
                } else {
                    self.pill().shake((elapsed.as_secs_f64() * 12.0 * WAVE).sin() * 6.0);
                    self.pill().render(Animation::Empty, t);
                }
            }
            Phase::Fading { since } => {
                let alpha = 1.0 - since.elapsed().as_secs_f64() / FADE;
                if alpha <= 0.0 {
                    self.pill().shake(0.0);
                    self.pill().hide();
                    self.pill().reset_mode();
                    self.ivars().mode_label_until.set(None);
                    self.pill().set_alpha(1.0);
                    self.ivars().pill_shown.set(false);
                    next = Some(Phase::Idle);
                } else {
                    self.pill().set_alpha(alpha);
                    self.pill().render(Animation::Empty, t);
                }
            }
        }

        if let Some(next) = next {
            let idle = matches!(next, Phase::Idle);
            *self.ivars().phase.borrow_mut() = next;
            if idle {
                self.stop_timer();
                self.update_status();
            }
        }
    }

    fn insert_take(&self, take: &Take) -> StoreOutcome {
        let (method, append_space, extras) = {
            let config = self.ivars().config.borrow();
            let mode_method = config.mode(&take.mode).and_then(|mode| mode.insertion);
            let method = mode_method
                .or_else(|| {
                    take.app_bundle_id.as_ref().and_then(|id| config.insertion.overrides.get(id).copied())
                })
                .unwrap_or(config.insertion.method);
            (method, config.insertion.append_space, insert_extras(&config))
        };
        let text = shape(&take.final_text, append_space);
        match insert::insert(&text, method, extras) {
            Ok(Outcome::Inserted) => StoreOutcome::Inserted,
            Ok(Outcome::CopiedSecure | Outcome::CopiedNoAccess) => StoreOutcome::Copied,
            Err(e) => {
                tf_core::log_error!("insertion failed: {e}");
                StoreOutcome::Copied
            }
        }
    }

    fn play(&self, sound: Sound) {
        let (enabled, style, volume) = {
            let config = self.ivars().config.borrow();
            (config.audio.sounds, config.audio.sound_style, config.audio.sound_volume)
        };
        if !enabled {
            return;
        }
        let name = match (style, sound) {
            (SoundStyle::Simple, Sound::Error) | (SoundStyle::Classic, Sound::Error) => ns_string!("Basso"),
            (SoundStyle::Simple, _) => ns_string!("Pop"),
            (SoundStyle::Classic, Sound::Start) => ns_string!("Tink"),
            (SoundStyle::Classic, _) => ns_string!("Pop"),
        };
        if let Some(sound) = NSSound::soundNamed(name) {
            sound.setVolume(volume.clamp(0.0, 1.0));
            sound.play();
            let mut playing = self.ivars().sounds.borrow_mut();
            playing.push(sound);
            if playing.len() > 4 {
                playing.remove(0);
            }
        }
    }

    pub(crate) fn preview_sound(&self) {
        self.play(Sound::Start);
    }

    /// öffnet die seite, die das diktieren freischaltet: erst rechte, dann ein sprachmodell.
    fn show_setup(&self) {
        let page = if permissions_ok() { Page::Models } else { Page::Permissions };
        self.show_settings(page);
    }

    pub(crate) fn permissions(&self) -> Permissions {
        let listen = CGPreflightListenEventAccess();
        let post = CGPreflightPostEventAccess();
        Permissions {
            microphone: match microphone_status() {
                status if status == AVAuthorizationStatus::Authorized => Access::Granted,
                status if status == AVAuthorizationStatus::NotDetermined => Access::NotAsked,
                _ => Access::Denied,
            },
            input_monitoring: Access::from(listen),
            accessibility: Access::from(post),
            restart_needed: self.ivars().tap_failed.get() && (listen || post),
        }
    }

    pub(crate) fn request_microphone(&self) {
        if microphone_status() != AVAuthorizationStatus::NotDetermined {
            open_privacy_pane("Privacy_Microphone");
            return;
        }
        // SAFETY: der medientyp ist eine framework-konstante; der completion-block ist leer, weil das einstellungsfenster das ergebnis pollt.
        let media_type = unsafe { AVMediaTypeAudio }.expect("AVMediaTypeAudio is available");
        let block = RcBlock::new(|_granted: Bool| {});
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block) };
    }

    pub(crate) fn open_input_monitoring(&self) {
        let _ = CGPreflightListenEventAccess();
        open_privacy_pane("Privacy_ListenEvent");
    }

    pub(crate) fn open_accessibility(&self) {
        let _ = CGPreflightPostEventAccess();
        open_privacy_pane("Privacy_Accessibility");
    }

    pub(crate) fn restart(&self) {
        let path = NSBundle::mainBundle().bundlePath().to_string();
        let _ = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 0.6; open \"$1\"")
            .arg("tf-restart")
            .arg(path)
            .spawn();
        NSApplication::sharedApplication(self.mtm()).terminate(None);
    }

    fn model_installed(&self, id: &str) -> bool {
        if id.starts_with("apple-speech:") {
            return true;
        }
        let dir = model_dir(id);
        dir.join(".installed.json").exists() || dir.join("Preprocessor.mlmodelc").exists()
    }

    pub(crate) fn update_status(&self) {
        let status = if let Some(started) = self.ivars().meeting_started.get() {
            let seconds = started.elapsed().as_secs();
            format!("Recording meeting · {:02}:{:02}", seconds / 60, seconds % 60)
        } else if let Some(failure) = self.ivars().engine_failure.borrow().as_ref() {
            if failure.is_model_missing() {
                "Model not installed".to_owned()
            } else {
                "Speech engine error".to_owned()
            }
        } else if !self.ivars().engine_ready.get() {
            "Loading speech model…".to_owned()
        } else if !permissions_ok() {
            "Permissions needed".to_owned()
        } else {
            let key = self.ivars().config.borrow().trigger.key;
            let mode = self.ivars().config.borrow().default_mode().name;
            format!("{mode} · Ready ({})", key.display())
        };
        self.set_status(&status);
    }

    fn set_status(&self, status: &str) {
        if let Some(item) = self.ivars().status_line.get() {
            item.setTitle(&NSString::from_str(status));
        }
    }

    fn set_toggle_title(&self, title: &str) {
        if let Some(item) = self.ivars().toggle_item.get() {
            item.setTitle(&NSString::from_str(title));
        }
    }

    fn start_timer(&self) {
        if self.ivars().timer.borrow().is_some() {
            return;
        }
        let this = self.retain();
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| this.tick());
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(FRAME_INTERVAL, true, &block) };
        *self.ivars().timer.borrow_mut() = Some(timer);
    }

    fn stop_timer(&self) {
        if let Some(timer) = self.ivars().timer.take() {
            timer.invalidate();
        }
    }

    fn elapsed(&self) -> f64 {
        self.ivars().anim_start.get().elapsed().as_secs_f64()
    }

    fn pill(&self) -> &Pill {
        self.ivars().pill.get().expect("pill created at launch")
    }

    fn build_status_item(&self, mtm: MainThreadMarker) -> Retained<NSStatusItem> {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("waveform"),
                Some(ns_string!("tf")),
            );
            button.setImage(icon.as_deref());
        }

        let menu = NSMenu::new(mtm);
        let status_line = menu_item(mtm, "Starting…", None, "");
        status_line.setEnabled(false);
        menu.addItem(&status_line);

        let toggle = menu_item(mtm, "Start Dictation", Some(sel!(toggleDictation:)), "");
        unsafe { toggle.setTarget(Some(self)) };
        menu.addItem(&toggle);

        let meeting = menu_item(mtm, "Start Meeting Recording…", Some(sel!(startMeeting:)), "");
        unsafe { meeting.setTarget(Some(self)) };
        menu.addItem(&meeting);
        let _ = self.ivars().meeting_item.set(meeting);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let recent = NSMenu::new(mtm);
        let recent_item = menu_item(mtm, "Recent", None, "");
        recent_item.setSubmenu(Some(&recent));
        menu.addItem(&recent_item);
        let _ = self.ivars().recent_menu.set(recent);

        let mode_menu = NSMenu::new(mtm);
        let mode_item = menu_item(mtm, "Mode", None, "");
        mode_item.setSubmenu(Some(&mode_menu));
        menu.addItem(&mode_item);
        let _ = self.ivars().mode_menu.set(mode_menu);
        self.refresh_mode_menu();

        let paste_last = menu_item(mtm, "Paste Last Transcript", Some(sel!(pasteLastTranscript:)), "v");
        paste_last.setKeyEquivalentModifierMask(NSEventModifierFlags::Control | NSEventModifierFlags::Option);
        unsafe { paste_last.setTarget(Some(self)) };
        menu.addItem(&paste_last);

        let history = menu_item(mtm, "History…", Some(sel!(showHistory:)), "y");
        unsafe { history.setTarget(Some(self)) };
        menu.addItem(&history);

        let meetings = menu_item(mtm, "Past Meetings…", Some(sel!(showMeetingHistory:)), "");
        unsafe { meetings.setTarget(Some(self)) };
        menu.addItem(&meetings);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let settings = menu_item(mtm, "Settings…", Some(sel!(showSettings:)), ",");
        unsafe { settings.setTarget(Some(self)) };
        menu.addItem(&settings);

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&menu_item(mtm, "Quit tf", Some(sel!(terminate:)), "q"));
        item.setMenu(Some(&menu));

        let _ = self.ivars().toggle_item.set(toggle);
        let _ = self.ivars().status_line.set(status_line);
        item
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Granted,
    Denied,
    NotAsked,
}

impl From<bool> for Access {
    fn from(granted: bool) -> Self {
        if granted { Access::Granted } else { Access::Denied }
    }
}

pub(crate) struct Permissions {
    pub microphone: Access,
    pub input_monitoring: Access,
    pub accessibility: Access,
    pub restart_needed: bool,
}

fn permissions_ok() -> bool {
    microphone_status() == AVAuthorizationStatus::Authorized
        && CGPreflightListenEventAccess()
        && CGPreflightPostEventAccess()
}

fn microphone_status() -> AVAuthorizationStatus {
    // SAFETY: avmediatypeaudio ist eine framework-konstante; hier wird nur tcc-status gelesen.
    let media_type = unsafe { AVMediaTypeAudio }.expect("AVMediaTypeAudio is available");
    unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) }
}

fn insert_extras(config: &Config) -> insert::Extras {
    insert::Extras {
        restore_clipboard: config.insertion.restore_clipboard,
        press_return: config.insertion.press_return,
    }
}

fn shape(text: &str, append_space: bool) -> String {
    let mut text = text.trim().to_owned();
    if append_space && !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push(' ');
    }
    text
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(60).collect::<String>())
    } else if line.is_empty() {
        "(empty)".to_owned()
    } else {
        line.to_owned()
    }
}

pub(crate) fn reveal_audio() {
    let dir = audio_dir();
    let fallback = support_dir();
    open_path(if dir.exists() { &dir } else { &fallback });
}

pub(crate) fn audio_size() -> u64 {
    fn size(path: &std::path::Path) -> u64 {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| match entry.metadata() {
                Ok(meta) if meta.is_dir() => size(&entry.path()),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            })
            .sum()
    }
    size(&audio_dir())
}

/// die alte `fetch-model`-ablage schrieb die dateien ohne `.installed.json`. solche verzeichnisse einmal übernehmen, damit registry und models-tab sie sehen.
fn adopt_legacy_models() {
    let Ok(catalog) = Catalog::embedded() else {
        return;
    };
    for entry in catalog.models.iter().filter(|entry| entry.kind == "stt") {
        let dir = model_dir(&entry.id);
        let marker = dir.join(".installed.json");
        if marker.exists() {
            continue;
        }
        let complete = entry.files.iter().all(|file| {
            std::fs::metadata(dir.join(&file.path)).map(|meta| meta.len() == file.size).unwrap_or(false)
        });
        if !complete {
            continue;
        }
        let text = format!(
            "{{\n  \"catalog_version\": {},\n  \"revision\": \"{}\",\n  \"installed_at\": 0\n}}\n",
            catalog.version, entry.source.revision
        );
        if let Err(e) = std::fs::write(&marker, text) {
            tf_core::log_warn!("could not adopt the legacy model {}: {e}", entry.id);
        }
    }
}

fn open_privacy_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) {
        let _ = NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

pub(crate) fn open_path(path: &std::path::Path) {
    let path = NSString::from_str(&path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&path);
    let _ = NSWorkspace::sharedWorkspace().openURL(&url);
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

/// aktiviert eine schon laufende tf-kopie. true heißt, dieser prozess soll sich beenden.
pub fn activate_existing() -> bool {
    let bundle = NSBundle::mainBundle();
    let Some(identifier) = bundle.bundleIdentifier() else {
        return false;
    };
    let me = std::process::id() as i32;
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
    for application in running.iter() {
        if application.processIdentifier() != me {
            application.activateWithOptions(NSApplicationActivationOptions::empty());
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::shape;

    #[test]
    fn appends_one_space() {
        assert_eq!(shape("  hello  ", true), "hello ");
        assert_eq!(shape("hello\n", true), "hello ");
        assert_eq!(shape("hello", false), "hello");
        assert_eq!(shape("   ", true), "");
    }
}
