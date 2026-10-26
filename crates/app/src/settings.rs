
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSButton, NSColor, NSColorSpace,
    NSColorWell, NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSImageView,
    NSLayoutConstraint, NSLayoutConstraintOrientation, NSLineBreakMode, NSModalResponseOK, NSOpenPanel,
    NSPopUpButton, NSProgressIndicator, NSScrollView, NSSlider, NSStackView, NSSwitch, NSTextAlignment,
    NSTextField, NSTextView, NSView, NSWindow, NSWorkspace,
};
use objc2_foundation::{NSArray, NSBundle, NSString, NSTimer};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use tf_apple::speech::SpeechLocale;
use tf_core::config::{CaptureMode, Config, HistoryLevel, InsertionMethod, SoundStyle};
use tf_core::mode::{self, Backend, Mode, ModeId, Refine};
use tf_core::paths::{history_path, models_dir, support_dir};
use tf_core::text::{Replacement, ReplacementKind, Term, validate_replacement};
use tf_core::trigger::{Style, TriggerKey};
use tf_models::{Catalog, CatalogEntry};
use tf_store::Store;

use crate::app::{Access, AppDelegate, DownloadPhase};
use crate::history::History;
use crate::shell::{Page, Shell};
use crate::ui::{self, Action, ShortcutRecorder};

const PERMISSION_POLL: Duration = Duration::from_secs(1);

type LocaleRx = Receiver<Result<Vec<SpeechLocale>, String>>;
type TestResult = Result<(String, u64), String>;
// TODO: refacto
type TestRx = Rc<RefCell<Option<Receiver<TestResult>>>>;

#[derive(Clone, Copy)]
struct Delegate(*const AppDelegate);

impl Delegate {
    fn get(self) -> &'static AppDelegate {
        // SAFETY: der delegate lebt für die ganze laufzeit der app.
        unsafe { &*self.0 }
    }
}

struct ModelRow {
    id: String,
    speech: bool,
    size: String,
    status: Retained<NSTextField>,
    action: Retained<NSButton>,
    default: Option<Retained<NSButton>>,
}

struct PermissionRow {
    status: Retained<NSTextField>,
    button: Retained<NSButton>,
}

struct ModeEditor {
    popup: Retained<NSPopUpButton>,
    name: Retained<NSTextField>,
    icon: Retained<NSPopUpButton>,
    engine: Retained<NSPopUpButton>,
    language: Retained<NSPopUpButton>,
    processing: Retained<NSPopUpButton>,
    model_popup: Retained<NSPopUpButton>,
    model_field: Retained<NSTextField>,
    model_hint: Retained<NSTextField>,
    prompt: Retained<NSTextView>,
    temperature: Retained<NSSlider>,
    temperature_value: Retained<NSTextField>,
    timeout: Retained<NSSlider>,
    timeout_value: Retained<NSTextField>,
    insertion: Retained<NSPopUpButton>,
    hotkey: Retained<ShortcutRecorder>,
    hotkey_value: Rc<Cell<TriggerKey>>,
    hotkey_enabled: Rc<Cell<bool>>,
    hotkey_check: Retained<NSSwitch>,
    apps: Rc<RefCell<Vec<String>>>,
    apps_label: Retained<NSTextField>,
    vocabulary: Retained<NSSwitch>,
    quick_switch: Retained<NSSwitch>,
    status: Retained<NSTextField>,
    test_input: Retained<NSTextField>,
    test_output: Retained<NSTextField>,
    ids: Rc<RefCell<Vec<ModeId>>>,
    cleanup: CleanupRows,
    advanced: Cell<bool>,
    advanced_button: Retained<NSButton>,
    test_section: Retained<NSView>,
    list_view: Retained<NSView>,
    detail_view: Retained<NSView>,
    list_stack: Retained<NSStackView>,
    list_actions: RefCell<Vec<Action>>,
    title: Retained<NSTextField>,
    title_icon: Retained<NSImageView>,
    active_button: Retained<NSButton>,
}

struct CleanupRows {
    model: Retained<NSView>,
    prompt: Retained<NSView>,
    advanced: Retained<NSView>,
    temperature: Retained<NSView>,
    timeout: Retained<NSView>,
}

pub struct Settings {
    shell: Shell,
    history: History,
    _actions: RefCell<Vec<Action>>,
    delegate: Delegate,
    shortcut: Retained<ShortcutRecorder>,
    switch_key: Retained<ShortcutRecorder>,
    switch_key_enabled: Retained<NSSwitch>,
    threshold: Retained<NSSlider>,
    threshold_value: Retained<NSTextField>,
    style: Retained<NSPopUpButton>,
    login: Retained<NSSwitch>,
    overrides: Retained<NSStackView>,
    override_actions: RefCell<Vec<Action>>,
    overrides_signature: RefCell<String>,
    devices: Rc<RefCell<Vec<tf_audio::InputDevice>>>,
    device_popup: Retained<NSPopUpButton>,
    level: Retained<NSProgressIndicator>,
    recorder: RefCell<Option<tf_audio::Recorder>>,
    recorder_retry: Cell<Instant>,
    last_device_refresh: Cell<Instant>,
    permissions: [PermissionRow; 3],
    restart: Retained<NSView>,
    last_permission_poll: Cell<Instant>,
    models: Vec<ModelRow>,
    apple_popup: Retained<NSPopUpButton>,
    apple_install: Retained<NSButton>,
    apple_default: Retained<NSButton>,
    apple_selected: Rc<RefCell<Option<String>>>,
    locales: RefCell<Vec<SpeechLocale>>,
    locales_rx: RefCell<Option<LocaleRx>>,
    mode_editor: Rc<ModeEditor>,
    modes_signature: RefCell<String>,
    test_rx: TestRx,
    vocab_terms: Retained<NSTextView>,
    vocab_replacements: Retained<NSTextView>,
    vocab_status: Retained<NSTextField>,
    history_store: Retained<NSPopUpButton>,
    history_text: Retained<NSPopUpButton>,
    history_audio: Retained<NSPopUpButton>,
    history_usage: Retained<NSTextField>,
    delete_history: Retained<NSButton>,
    meeting_detect: Retained<NSSwitch>,
    meeting_capture: Retained<NSPopUpButton>,
    meeting_summary: Retained<NSPopUpButton>,
    meeting_summary_endpoint_model: Retained<NSTextField>,
    meeting_summary_prompt: Retained<NSTextView>,
    meeting_keep: Retained<NSSwitch>,
    meeting_echo: Retained<NSSwitch>,
    meeting_identify: Retained<NSSwitch>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

struct GeneralPage {
    view: Retained<objc2_app_kit::NSScrollView>,
    shortcut: Retained<ShortcutRecorder>,
    switch_key: Retained<ShortcutRecorder>,
    switch_key_enabled: Retained<NSSwitch>,
    style: Retained<NSPopUpButton>,
    threshold: Retained<NSSlider>,
    threshold_value: Retained<NSTextField>,
    login: Retained<NSSwitch>,
    overrides: Retained<NSStackView>,
}

struct AudioPage {
    view: Retained<objc2_app_kit::NSScrollView>,
    device_popup: Retained<NSPopUpButton>,
    level: Retained<NSProgressIndicator>,
}

struct PermissionsPage {
    view: Retained<objc2_app_kit::NSScrollView>,
    rows: [PermissionRow; 3],
    restart: Retained<NSView>,
}

struct ModelsPage {
    view: Retained<objc2_app_kit::NSScrollView>,
    models: Vec<ModelRow>,
    apple_popup: Retained<NSPopUpButton>,
    apple_install: Retained<NSButton>,
    apple_default: Retained<NSButton>,
}

struct ModesPage {
    view: Retained<NSView>,
    editor: Rc<ModeEditor>,
}

struct VocabularyPage {
    view: Retained<objc2_app_kit::NSScrollView>,
    terms: Retained<NSTextView>,
    replacements: Retained<NSTextView>,
    status: Retained<NSTextField>,
}

struct StoragePage {
    view: Retained<NSScrollView>,
    store: Retained<NSPopUpButton>,
    text: Retained<NSPopUpButton>,
    audio: Retained<NSPopUpButton>,
    usage: Retained<NSTextField>,
    delete: Retained<NSButton>,
}

struct MeetingsPage {
    view: Retained<NSScrollView>,
    detect: Retained<NSSwitch>,
    capture: Retained<NSPopUpButton>,
    summary: Retained<NSPopUpButton>,
    summary_endpoint_model: Retained<NSTextField>,
    summary_prompt: Retained<NSTextView>,
    keep: Retained<NSSwitch>,
    echo: Retained<NSSwitch>,
    identify: Retained<NSSwitch>,
}

/// sammelt die actions einer seite, damit das fenster sie am leben hält.
struct Actions<'a>(&'a RefCell<Vec<Action>>);

impl Actions<'_> {
    fn keep(&self, action: Action) {
        self.0.borrow_mut().push(action);
    }
}

fn hex_color(hex: &str) -> Retained<NSColor> {
    let channel = |i: usize| {
        hex.get(i..i + 2).and_then(|v| u8::from_str_radix(v, 16).ok()).map_or(0.5, |v| f64::from(v) / 255.0)
    };
    NSColor::colorWithSRGBRed_green_blue_alpha(channel(1), channel(3), channel(5), 1.0)
}

fn color_hex(color: &NSColor) -> Option<String> {
    let rgb = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(format!(
        "#{:02X}{:02X}{:02X}",
        byte(rgb.redComponent()),
        byte(rgb.greenComponent()),
        byte(rgb.blueComponent())
    ))
}

fn popup_changed(mtm: MainThreadMarker, handler: impl Fn(isize) + 'static) -> Action {
    Action::new(mtm, move |sender: &AnyObject| {
        if let Some(popup) = sender.downcast_ref::<NSPopUpButton>() {
            handler(popup.indexOfSelectedItem());
        }
    })
}

fn toggled(mtm: MainThreadMarker, handler: impl Fn(bool) + 'static) -> Action {
    Action::new(mtm, move |sender: &AnyObject| {
        if let Some(toggle) = sender.downcast_ref::<NSSwitch>() {
            handler(toggle.state() == NSControlStateValueOn);
        }
    })
}

fn clicked(mtm: MainThreadMarker, handler: impl Fn() + 'static) -> Action {
    Action::new(mtm, move |_sender: &AnyObject| handler())
}

/// eine modifier-taste für den mode-switch, die nicht mit dem diktat-shortcut kollidiert.
fn default_switch_key(trigger: TriggerKey) -> TriggerKey {
    if trigger == TriggerKey::RightControl { TriggerKey::RightCommand } else { TriggerKey::RightControl }
}

fn switch_key_taken(config: &Config, key: TriggerKey) -> bool {
    key == config.trigger.key || config.modes().iter().any(|mode| mode.hotkey == Some(key))
}

fn state(on: bool) -> isize {
    if on { NSControlStateValueOn } else { NSControlStateValueOff }
}

impl Settings {
    pub fn new(mtm: MainThreadMarker, delegate: &AppDelegate) -> Self {
        let delegate = Delegate(delegate as *const AppDelegate);
        let config = delegate.get().config();
        let actions = RefCell::new(Vec::new());
        let keep = Actions(&actions);
        let devices = Rc::new(RefCell::new(tf_audio::devices()));
        let apple_selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let test_rx: TestRx = Rc::new(RefCell::new(None));

        let general = general_page(mtm, delegate, &config, &keep);
        let audio = audio_page(mtm, delegate, &config, &devices, &keep);
        let permissions = permissions_page(mtm, delegate, &keep);
        let models = models_page(mtm, delegate, &config, &apple_selected, &keep);
        let modes = modes_page(mtm, delegate, &test_rx, &keep);
        let vocabulary = vocabulary_page(mtm, delegate, &keep);
        let meetings = meetings_page(mtm, delegate, &config, &keep);
        let storage = storage_page(mtm, delegate, &config, &keep);
        let history = History::new(mtm, delegate.get());
        let about = about_page(mtm, delegate, &keep);

        let shell = Shell::new(
            mtm,
            vec![
                as_view(&general.view),
                as_view(&permissions.view),
                as_view(&audio.view),
                as_view(&models.view),
                modes.view.clone(),
                as_view(&vocabulary.view),
                as_view(&meetings.view),
                history.view().clone(),
                as_view(&storage.view),
                as_view(&about),
            ],
        );

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tf-speech-locales".into())
            .spawn(move || {
                let _ = tx.send(tf_apple::speech::locales().map_err(|e| e.to_string()));
            })
            .ok();

        let settings = Self {
            shell,
            history,
            _actions: actions,
            delegate,
            shortcut: general.shortcut,
            switch_key: general.switch_key,
            switch_key_enabled: general.switch_key_enabled,
            threshold: general.threshold,
            threshold_value: general.threshold_value,
            style: general.style,
            login: general.login,
            overrides: general.overrides,
            override_actions: RefCell::new(Vec::new()),
            overrides_signature: RefCell::new(String::new()),
            devices,
            device_popup: audio.device_popup,
            level: audio.level,
            recorder: RefCell::new(None),
            recorder_retry: Cell::new(Instant::now()),
            last_device_refresh: Cell::new(Instant::now() - Duration::from_secs(10)),
            permissions: permissions.rows,
            restart: permissions.restart,
            last_permission_poll: Cell::new(Instant::now()),
            models: models.models,
            apple_popup: models.apple_popup,
            apple_install: models.apple_install,
            apple_default: models.apple_default,
            apple_selected,
            locales: RefCell::new(Vec::new()),
            locales_rx: RefCell::new(Some(rx)),
            mode_editor: modes.editor,
            modes_signature: RefCell::new(String::new()),
            test_rx,
            vocab_terms: vocabulary.terms,
            vocab_replacements: vocabulary.replacements,
            vocab_status: vocabulary.status,
            history_store: storage.store,
            history_text: storage.text,
            history_audio: storage.audio,
            history_usage: storage.usage,
            delete_history: storage.delete,
            meeting_detect: meetings.detect,
            meeting_capture: meetings.capture,
            meeting_summary: meetings.summary,
            meeting_summary_endpoint_model: meetings.summary_endpoint_model,
            meeting_summary_prompt: meetings.summary_prompt,
            meeting_keep: meetings.keep,
            meeting_echo: meetings.echo,
            meeting_identify: meetings.identify,
            timer: RefCell::new(None),
        };
        settings.rebuild_overrides();
        settings.refresh_devices();
        settings.refresh_modes();
        settings.load_vocabulary();
        settings.history.reload();
        settings.refresh();
        settings
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn window(&self) -> &NSWindow {
        self.shell.window()
    }

    pub fn show(&self, page: Page) {
        self.shell.select(page);
        self.shell.window().makeKeyAndOrderFront(None);
        self.start_timer();
    }

    pub fn stop(&self) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
        self.recorder.take();
        self.shortcut.cancel_recording();
        self.switch_key.cancel_recording();
    }

    pub fn refresh(&self) {
        let config = self.delegate.get().config();
        let signature = config
            .insertion
            .overrides
            .iter()
            .map(|(bundle, method)| format!("{bundle}={method:?};"))
            .collect::<String>();
        if *self.overrides_signature.borrow() != signature {
            *self.overrides_signature.borrow_mut() = signature;
            self.rebuild_overrides();
        }
        self.shortcut.set_current(config.trigger.key);
        self.switch_key.set_current(
            config.trigger.mode_switch_key.unwrap_or_else(|| default_switch_key(config.trigger.key)),
        );
        self.switch_key_enabled.setState(state(config.trigger.mode_switch_key.is_some()));
        self.threshold.setDoubleValue(f64::from(config.trigger.hold_threshold_ms));
        self.threshold_value
            .setStringValue(&NSString::from_str(&format!("{} ms", config.trigger.hold_threshold_ms)));
        self.threshold.setEnabled(config.trigger.style == Style::Hybrid);
        self.style.selectItemAtIndex(match config.trigger.style {
            Style::Hybrid => 0,
            Style::Hold => 1,
            Style::Toggle => 2,
        });
        self.login.setState(state(launch_at_login()));
        self.refresh_permissions();
        self.refresh_models(&config);
        self.refresh_modes();
        self.refresh_history(&config);
        self.refresh_meetings(&config);
    }

    fn refresh_meetings(&self, config: &Config) {
        self.meeting_detect.setState(state(config.meetings.detect));
        self.meeting_capture.selectItemAtIndex(match config.meetings.capture {
            CaptureMode::All => 0,
            CaptureMode::App => 1,
        });
        let choices = summary_choices(config);
        let labels = choices.iter().map(|(label, _, _)| label.as_str()).collect::<Vec<_>>();
        fill_popup(&self.meeting_summary, &labels, summary_choice_index(&choices, &config.meetings.summary));
        let endpoint = matches!(config.meetings.summary.backend, Backend::Endpoint(_));
        self.meeting_summary_endpoint_model.setEnabled(endpoint);
        self.meeting_summary_endpoint_model.setStringValue(&NSString::from_str(
            config.meetings.summary.model.as_deref().filter(|_| endpoint).unwrap_or(""),
        ));
        set_text(&self.meeting_summary_prompt, &config.meetings.summary.prompt);
        self.meeting_keep.setState(state(config.meetings.keep_audio));
        self.meeting_echo.setState(state(config.meetings.echo_cancellation));
        self.meeting_identify.setState(state(config.meetings.identify_speakers));
    }

    fn refresh_modes(&self) {
        let config = self.delegate.get().config();
        let signature = serde_json::to_string(&config.modes()).unwrap_or_default()
            + &config.default_mode_id
            + &config.engine.default
            + &llm_options().iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>().join(",");
        if *self.modes_signature.borrow() == signature {
            return;
        }
        *self.modes_signature.borrow_mut() = signature;
        let editor = &self.mode_editor;
        let selected = editor
            .ids
            .borrow()
            .get(editor.popup.indexOfSelectedItem().max(0) as usize)
            .cloned()
            .unwrap_or_else(|| config.default_mode_id.clone());
        refresh_mode_popup(editor, &config, &selected);
        let mtm = MainThreadMarker::new().expect("settings runs on the main thread");
        rebuild_mode_list(mtm, self.delegate, editor);
    }

    fn load_vocabulary(&self) {
        let config = self.delegate.get().config();
        let terms = config
            .vocabulary
            .terms
            .iter()
            .map(|term| if term.exact { format!("={}", term.text) } else { term.text.clone() })
            .collect::<Vec<_>>()
            .join("\n");
        set_text(&self.vocab_terms, &terms);
        let replacements = config
            .vocabulary
            .replacements
            .iter()
            .map(|replacement| {
                let from = if replacement.kind == ReplacementKind::Regex {
                    format!("/{}/", replacement.from)
                } else {
                    replacement.from.clone()
                };
                format!("{from} => {}", replacement.to.replace('\n', "\\n"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        set_text(&self.vocab_replacements, &replacements);
        self.vocab_status.setStringValue(&NSString::from_str(""));
    }

    fn poll_test(&self) {
        let result = {
            let rx = self.test_rx.borrow();
            rx.as_ref().and_then(|rx| rx.try_recv().ok())
        };
        let Some(result) = result else {
            return;
        };
        self.test_rx.borrow_mut().take();
        match result {
            Ok((text, ms)) => {
                self.mode_editor
                    .test_output
                    .setStringValue(&NSString::from_str(&format!("{text}  ·  {ms} ms")));
                self.mode_editor.test_output.setTextColor(Some(&NSColor::labelColor()));
            }
            Err(e) => {
                self.mode_editor.test_output.setStringValue(&NSString::from_str(&e));
                self.mode_editor.test_output.setTextColor(Some(&NSColor::systemRedColor()));
            }
        }
    }

    fn start_timer(&self) {
        if self.timer.borrow().is_some() {
            return;
        }
        let ptr = self as *const Self;
        let block = RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            // SAFETY: der delegate besitzt die settings für die ganze laufzeit der app.
            unsafe { &*ptr }.tick();
        });
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &block)
        };
        *self.timer.borrow_mut() = Some(timer);
    }

    fn tick(&self) {
        self.poll_locales();
        self.poll_test();
        if self.shell.selected() == Page::Permissions
            && self.last_permission_poll.get().elapsed() >= PERMISSION_POLL
        {
            self.last_permission_poll.set(Instant::now());
            self.refresh_permissions();
        }
        if self.shell.selected() == Page::History {
            self.history.tick();
        }
        if self.shell.selected() == Page::Audio {
            if self.recorder.borrow().is_none() && Instant::now() >= self.recorder_retry.get() {
                let device = self.delegate.get().config().audio.device;
                match tf_audio::Recorder::start(device.as_deref()) {
                    Ok((recorder, _)) => *self.recorder.borrow_mut() = Some(recorder),
                    Err(_) => {
                        self.recorder_retry.set(Instant::now() + Duration::from_secs(2));
                    }
                }
            }
            if let Some(recorder) = self.recorder.borrow().as_ref() {
                self.level.setDoubleValue(f64::from(recorder.level()));
            }
            if self.last_device_refresh.get().elapsed() > Duration::from_secs(2) {
                self.last_device_refresh.set(Instant::now());
                self.refresh_devices();
            }
        } else {
            self.recorder.take();
            self.level.setDoubleValue(0.0);
        }
    }

    fn poll_locales(&self) {
        let received = {
            let rx = self.locales_rx.borrow();
            rx.as_ref().and_then(|rx| rx.try_recv().ok())
        };
        let Some(result) = received else {
            return;
        };
        self.locales_rx.borrow_mut().take();
        match result {
            Ok(locales) => {
                let current = self
                    .delegate
                    .get()
                    .config()
                    .engine
                    .default
                    .strip_prefix("apple-speech:")
                    .map(str::to_owned);
                let selected = current
                    .as_ref()
                    .and_then(|id| locales.iter().position(|locale| &locale.id == id))
                    .or_else(|| locales.iter().position(|locale| locale.installed))
                    .unwrap_or(0);
                *self.locales.borrow_mut() = locales;
                self.populate_locales(selected);
            }
            Err(e) => {
                self.apple_popup.removeAllItems();
                self.apple_popup.addItemWithTitle(&NSString::from_str("Unavailable"));
                self.apple_popup.setEnabled(false);
                self.apple_popup
                    .setToolTip(Some(&NSString::from_str(&format!("Apple Speech is unavailable: {e}"))));
            }
        }
    }

    fn populate_locales(&self, selected: usize) {
        self.apple_popup.removeAllItems();
        let locales = self.locales.borrow();
        for locale in locales.iter() {
            let mark = if locale.installed { "✓ " } else { "" };
            self.apple_popup.addItemWithTitle(&NSString::from_str(&format!("{mark}{}", locale.name)));
        }
        if !locales.is_empty() {
            self.apple_popup.selectItemAtIndex(selected.min(locales.len() - 1) as isize);
        }
    }

    fn refresh_devices(&self) {
        let devices = tf_audio::devices();
        let pinned = self.delegate.get().config().audio.device.clone();
        self.device_popup.removeAllItems();
        self.device_popup.addItemWithTitle(&NSString::from_str("System default"));
        let mut selected = 0;
        for (index, device) in devices.iter().enumerate() {
            let title =
                if device.is_default { format!("{} (default)", device.name) } else { device.name.clone() };
            self.device_popup.addItemWithTitle(&NSString::from_str(&title));
            if pinned.as_deref() == Some(device.id.as_str()) {
                selected = index + 1;
            }
        }
        if pinned.is_some() && selected == 0 {
            self.device_popup.addItemWithTitle(&NSString::from_str("Selected microphone (not connected)"));
            selected = devices.len() + 1;
        }
        self.device_popup.selectItemAtIndex(selected as isize);
        *self.devices.borrow_mut() = devices;
    }

    fn rebuild_overrides(&self) {
        for view in self.overrides.arrangedSubviews().iter() {
            self.overrides.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        let config = self.delegate.get().config();
        let mtm = MainThreadMarker::new().expect("settings runs on the main thread");
        let delegate = self.delegate;
        let mut actions = Vec::new();

        let add = clicked(mtm, move || {
            if let Some((_, bundle)) = pick_app(mtm) {
                delegate.get().update_config(|config| {
                    config.insertion.overrides.insert(bundle, InsertionMethod::Paste);
                });
                delegate.get().refresh_settings();
            }
        });
        let add_button = ui::button(mtm, "Add App…", &add);
        actions.push(add);
        let mut rows = vec![ui::row(
            mtm,
            "Apps with their own method",
            Some("Override how text is inserted in a specific app."),
            &[],
            ui::views![add_button],
        )];

        for (bundle, method) in &config.insertion.overrides {
            let selected = match method {
                InsertionMethod::Paste => 0,
                InsertionMethod::Type => 1,
            };
            let bundle_owned = bundle.clone();
            let action = popup_changed(mtm, move |index| {
                let method = if index == 1 { InsertionMethod::Type } else { InsertionMethod::Paste };
                let bundle = bundle_owned.clone();
                delegate.get().update_config(|config| {
                    config.insertion.overrides.insert(bundle, method);
                });
            });
            let popup = ui::popup(mtm, &["Paste", "Type"], selected, &action);
            actions.push(action);

            let bundle_owned = bundle.clone();
            let action = clicked(mtm, move || {
                let bundle = bundle_owned.clone();
                delegate.get().update_config(|config| {
                    config.insertion.overrides.remove(&bundle);
                });
                delegate.get().refresh_settings();
            });
            let remove = ui::button(mtm, "−", &action);
            actions.push(action);
            rows.push(ui::row(mtm, bundle, None, &[], ui::views![popup, remove]));
        }

        let section = ui::section(mtm, Some("App overrides"), &rows, None);
        self.overrides.addArrangedSubview(&section);
        ui::fill_width(&self.overrides, &section, 0.0);
        *self.override_actions.borrow_mut() = actions;
    }

    fn refresh_permissions(&self) {
        let permissions = self.delegate.get().permissions();
        let access = [permissions.microphone, permissions.input_monitoring, permissions.accessibility];
        for (row, access) in self.permissions.iter().zip(access) {
            let (text, color, button) = match access {
                Access::Granted => ("✓ Granted", NSColor::systemGreenColor(), "Open…"),
                Access::Denied => ("⚠︎ Not granted", NSColor::systemOrangeColor(), "Grant Access…"),
                Access::NotAsked => ("Not asked yet", NSColor::secondaryLabelColor(), "Grant Access…"),
            };
            row.status.setStringValue(&NSString::from_str(text));
            row.status.setTextColor(Some(&color));
            row.button.setTitle(&NSString::from_str(button));
        }
        self.restart.setHidden(!permissions.restart_needed);
        self.delegate.get().update_status();
    }

    fn refresh_history(&self, config: &Config) {
        self.history_store.selectItemAtIndex(history_index(config.history.store) as isize);
        self.history_text.selectItemAtIndex(retention_index(config.history.retention_days) as isize);
        self.history_audio.selectItemAtIndex(retention_index(config.history.audio_retention_days) as isize);
        self.history_audio.setEnabled(config.history.store.stores_audio());

        let takes = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.stats().ok())
            .map(|stats| stats.takes)
            .unwrap_or(0);
        let bytes = crate::app::audio_size();
        self.history_usage.setStringValue(&NSString::from_str(&format!(
            "{} dictations · {:.1} MB audio",
            thousands(takes),
            bytes as f64 / 1_000_000.0
        )));
        self.delete_history.setEnabled(takes > 0);
    }

    fn refresh_models(&self, config: &Config) {
        let installed_ids = installed_model_ids();
        let download = self.delegate.get().download_view();
        for row in &self.models {
            let is_installed = installed_ids.contains(&row.id);
            let view = download.as_ref().filter(|view| view.id == row.id);
            let active = view.filter(|view| {
                !matches!(
                    view.phase,
                    DownloadPhase::Installed | DownloadPhase::Failed(_) | DownloadPhase::Cancelled
                )
            });
            let busy = matches!(view.map(|view| &view.phase), Some(DownloadPhase::Optimizing));
            let status = match view.map(|view| &view.phase) {
                Some(DownloadPhase::Downloading) => format!("{:.0}%", view.unwrap().progress * 100.0),
                Some(DownloadPhase::Verifying) => "Verifying…".to_owned(),
                Some(DownloadPhase::Optimizing) if row.speech => "Optimizing…".to_owned(),
                Some(DownloadPhase::Optimizing) => "Installing…".to_owned(),
                Some(DownloadPhase::Installed) => "Installed".to_owned(),
                Some(DownloadPhase::Failed(_)) => "Failed".to_owned(),
                Some(DownloadPhase::Cancelled) | None if !is_installed => row.size.clone(),
                _ => "Installed".to_owned(),
            };
            row.status.setStringValue(&NSString::from_str(&status));
            let failure = match view.map(|view| &view.phase) {
                Some(DownloadPhase::Failed(e)) => Some(NSString::from_str(e)),
                _ => None,
            };
            row.status.setToolTip(failure.as_deref());
            let failed = failure.is_some();
            ui::set_status_color(&row.status, failed);
            if active.is_some() {
                ui::set_symbol(&row.action, "xmark.circle", "Cancel", &NSColor::secondaryLabelColor());
            } else if is_installed {
                ui::set_symbol(&row.action, "trash", "Remove", &NSColor::secondaryLabelColor());
            } else {
                ui::set_symbol(&row.action, "arrow.down.circle", "Download", &NSColor::secondaryLabelColor());
            }
            row.action.setEnabled(!busy);
            if let Some(default) = &row.default {
                default.setEnabled(is_installed);
                ui::set_star(default, config.engine.default == row.id);
            }
        }

        let apple_default = config.engine.default.strip_prefix("apple-speech:").map(str::to_owned);
        let locales = self.locales.borrow();
        let selected = self.apple_popup.indexOfSelectedItem();
        let locale = if selected >= 0 { locales.get(selected as usize) } else { None };
        *self.apple_selected.borrow_mut() = locale.map(|locale| locale.id.clone());
        let installed_locale = locale.map(|locale| locale.installed).unwrap_or(false);
        let apple_active = download.as_ref().filter(|view| {
            view.id.starts_with("apple-speech:")
                && !matches!(
                    view.phase,
                    DownloadPhase::Installed | DownloadPhase::Failed(_) | DownloadPhase::Cancelled
                )
        });
        if installed_locale {
            ui::set_symbol(
                &self.apple_install,
                "checkmark.circle.fill",
                "Installed",
                &NSColor::systemGreenColor(),
            );
        } else {
            ui::set_symbol(
                &self.apple_install,
                "arrow.down.circle",
                "Install language",
                &NSColor::secondaryLabelColor(),
            );
        }
        self.apple_install.setEnabled(!installed_locale && apple_active.is_none());
        self.apple_default.setEnabled(installed_locale);
        ui::set_star(&self.apple_default, apple_default.is_some());
    }
}

fn as_view(scroll: &Retained<objc2_app_kit::NSScrollView>) -> Retained<NSView> {
    Retained::into_super(scroll.clone())
}

fn general_page(mtm: MainThreadMarker, delegate: Delegate, config: &Config, keep: &Actions) -> GeneralPage {
    let shortcut =
        ShortcutRecorder::new(mtm, config.trigger.key, delegate.get().hotkey_paused(), move |key| {
            delegate.get().update_config(|config| config.trigger.key = key);
        });

    let switch_key = ShortcutRecorder::new(
        mtm,
        config.trigger.mode_switch_key.unwrap_or_else(|| default_switch_key(config.trigger.key)),
        delegate.get().hotkey_paused(),
        move |key| {
            let config = delegate.get().config();
            if switch_key_taken(&config, key) {
                delegate.get().refresh_settings();
                return;
            }
            delegate.get().update_config(|config| config.trigger.mode_switch_key = Some(key));
            delegate.get().refresh_settings();
        },
    );
    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| {
            config.trigger.mode_switch_key = on.then(|| default_switch_key(config.trigger.key));
        });
        delegate.get().refresh_settings();
    });
    let switch_key_enabled = ui::toggle(mtm, config.trigger.mode_switch_key.is_some(), &action);
    keep.keep(action);

    let action = popup_changed(mtm, move |index| {
        let style = match index {
            1 => Style::Hold,
            2 => Style::Toggle,
            _ => Style::Hybrid,
        };
        delegate.get().update_config(|config| config.trigger.style = style);
        delegate.get().refresh_settings();
    });
    let style_index = match config.trigger.style {
        Style::Hybrid => 0,
        Style::Hold => 1,
        Style::Toggle => 2,
    };
    let style =
        ui::popup(mtm, &["Hybrid (tap or hold)", "Hold to talk", "Tap to toggle"], style_index, &action);
    ui::sized(&style, Some(200.0), None);
    keep.keep(action);

    let action = Action::new(mtm, move |sender: &AnyObject| {
        if let Some(slider) = sender.downcast_ref::<NSSlider>() {
            let value = slider.doubleValue().round() as u32;
            delegate.get().update_config(|config| config.trigger.hold_threshold_ms = value);
        }
    });
    let threshold = ui::slider(mtm, 150.0..=600.0, f64::from(config.trigger.hold_threshold_ms), &action);
    let threshold_value = ui::value_label(mtm, &format!("{} ms", config.trigger.hold_threshold_ms));
    ui::sized(&threshold_value, Some(52.0), None);
    keep.keep(action);

    let dictation = ui::section(
        mtm,
        Some("Dictation"),
        &[
            ui::row(
                mtm,
                "Shortcut",
                Some("The key or key combination you hold or tap to start dictating."),
                &[],
                ui::views![shortcut],
            ),
            ui::row(
                mtm,
                "Mode switch key",
                Some(
                    "While recording, press this key to switch to the next mode. Off uses Tab (Shift-Tab goes back). It does nothing when idle.",
                ),
                &[],
                ui::views![switch_key, switch_key_enabled],
            ),
            ui::row(
                mtm,
                "Trigger style",
                Some(
                    "Hybrid: tap to start and stop, or hold to talk. Hold to talk records only while the key is down. Tap to toggle starts and stops on each press.",
                ),
                &[],
                ui::views![style],
            ),
            ui::row(
                mtm,
                "Hold threshold",
                Some("How long to hold before a press counts as hold to talk."),
                &[],
                ui::views![threshold, threshold_value],
            ),
        ],
        None,
    );

    let action = popup_changed(mtm, move |index| {
        let method = if index == 1 { InsertionMethod::Type } else { InsertionMethod::Paste };
        delegate.get().update_config(|config| config.insertion.method = method);
    });
    let method_index = match config.insertion.method {
        InsertionMethod::Paste => 0,
        InsertionMethod::Type => 1,
    };
    let method =
        ui::popup(mtm, &["Paste (restores the clipboard)", "Type the characters"], method_index, &action);
    ui::sized(&method, Some(240.0), None);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.insertion.append_space = on);
    });
    let append_space = ui::toggle(mtm, config.insertion.append_space, &action);
    keep.keep(action);

    let action = popup_changed(mtm, move |index| {
        delegate.get().update_config(|config| config.insertion.restore_clipboard = index != 1);
    });
    let clipboard = ui::popup(
        mtm,
        &["Keep what I copied", "Replace with dictation"],
        usize::from(!config.insertion.restore_clipboard),
        &action,
    );
    ui::sized(&clipboard, Some(240.0), None);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.insertion.press_return = on);
    });
    let press_return = ui::toggle(mtm, config.insertion.press_return, &action);
    keep.keep(action);

    let insertion = ui::section(
        mtm,
        Some("Text input"),
        &[
            ui::row(
                mtm,
                "Method",
                Some(
                    "Paste puts the text on the clipboard and presses ⌘V, then restores your clipboard. Type sends the characters as keystrokes, which works in apps that block paste.",
                ),
                &[],
                ui::views![method],
            ),
            ui::row(
                mtm,
                "Add a space after dictation",
                Some("Appends a space after the text so you can keep dictating without typing one."),
                &[],
                ui::views![append_space],
            ),
            ui::row(
                mtm,
                "Clipboard content",
                Some(
                    "Keep puts your clipboard back after pasting. Replace leaves the dictated text on the clipboard.",
                ),
                &[],
                ui::views![clipboard],
            ),
            ui::row(
                mtm,
                "Press Return after inserting",
                Some("Sends Return once the text is in, for example to submit a chat message."),
                &[],
                ui::views![press_return],
            ),
        ],
        None,
    );

    let overrides = ui::vstack(mtm, 0.0);

    let action = toggled(mtm, move |on| {
        if let Err(e) = set_launch_at_login(on) {
            tf_core::log_error!("launch at login: {e}");
        }
    });
    let login = ui::toggle(mtm, launch_at_login(), &action);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.app.show_in_dock = on);
    });
    let dock = ui::toggle(mtm, config.app.show_in_dock, &action);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.app.processing_glow = on);
    });
    let glow = ui::toggle(mtm, config.app.processing_glow, &action);
    keep.keep(action);

    let action = Action::new(mtm, move |sender: &AnyObject| {
        if let Some(well) = sender.downcast_ref::<NSColorWell>()
            && let Some(hex) = color_hex(&well.color())
        {
            delegate.get().update_config(|config| config.app.glow_color = hex);
        }
    });
    let glow_color = NSColorWell::new(mtm);
    glow_color.setColor(&hex_color(&config.app.glow_color));
    action.attach(&glow_color);
    ui::sized(&glow_color, Some(44.0), Some(24.0));
    keep.keep(action);

    let behaviour = ui::section(
        mtm,
        Some("Application"),
        &[
            ui::row(
                mtm,
                "Show in Dock",
                Some("Shows tf in the Dock and the app switcher as well as the menu bar."),
                &[],
                ui::views![dock],
            ),
            ui::row(
                mtm,
                "Launch at login",
                Some("Starts tf automatically when you log in."),
                &[],
                ui::views![login],
            ),
            ui::row(
                mtm,
                "Processing glow",
                Some("A light runs around the pill while your recording is transcribed."),
                &[],
                ui::views![glow],
            ),
            ui::row(mtm, "Glow color", None, &[], ui::views![glow_color]),
        ],
        None,
    );

    let durations: [(&str, u32); 5] =
        [("1 minute", 60), ("5 minutes", 300), ("15 minutes", 900), ("1 hour", 3600), ("Always", 0)];
    let duration_index =
        durations.iter().position(|(_, secs)| *secs == config.engine.unload_after_secs).unwrap_or(4);
    let action = popup_changed(mtm, move |index| {
        let secs = durations.get(index.max(0) as usize).map(|(_, secs)| *secs).unwrap_or(0);
        delegate.get().update_config(|config| config.engine.unload_after_secs = secs);
    });
    let duration = ui::popup(mtm, &durations.map(|(label, _)| label), duration_index, &action);
    ui::sized(&duration, Some(200.0), None);
    keep.keep(action);
    let voice_model = ui::section(
        mtm,
        Some("Voice model"),
        &[ui::row(
            mtm,
            "Voice model active duration",
            Some(
                "How long the speech model stays in memory after your last dictation. \"Always\" keeps it loaded for instant starts; a shorter time frees memory, and the next dictation then starts a little slower.",
            ),
            &[],
            ui::views![duration],
        )],
        None,
    );

    let overrides_view: Retained<NSView> = Retained::into_super(overrides.clone());
    GeneralPage {
        view: ui::form(mtm, &[behaviour, dictation, voice_model, insertion, overrides_view]),
        shortcut,
        switch_key,
        switch_key_enabled,
        style,
        threshold,
        threshold_value,
        login,
        overrides,
    }
}

fn audio_page(
    mtm: MainThreadMarker,
    delegate: Delegate,
    config: &Config,
    devices: &Rc<RefCell<Vec<tf_audio::InputDevice>>>,
    keep: &Actions,
) -> AudioPage {
    let device_ref = Rc::clone(devices);
    let action = popup_changed(mtm, move |index| {
        let uid = if index <= 0 {
            None
        } else {
            device_ref.borrow().get(index as usize - 1).map(|device| device.id.clone())
        };
        delegate.get().update_config(|config| config.audio.device = uid);
    });
    let device_popup = ui::popup(mtm, &["System default"], 0, &action);
    ui::sized(&device_popup, Some(260.0), None);
    keep.keep(action);

    let level = ui::progress(mtm);
    ui::sized(&level, Some(220.0), None);

    let input = ui::section(
        mtm,
        Some("Input"),
        &[
            ui::row(
                mtm,
                "Microphone",
                None,
                &[&ui::detail(
                    mtm,
                    "The pinned microphone is used whenever it is connected. If it is unplugged, \
                     tf falls back to the system default for that take.",
                )],
                ui::views![device_popup],
            ),
            ui::row(
                mtm,
                "Live level",
                Some("The input opens only while this page is visible."),
                &[],
                ui::views![level],
            ),
        ],
        None,
    );

    let sensitivity = match config.audio.vad {
        tf_audio::vad::Sensitivity::Low => 0,
        tf_audio::vad::Sensitivity::Normal => 1,
        tf_audio::vad::Sensitivity::High => 2,
    };
    let action = popup_changed(mtm, move |index| {
        let sensitivity = match index {
            0 => tf_audio::vad::Sensitivity::Low,
            2 => tf_audio::vad::Sensitivity::High,
            _ => tf_audio::vad::Sensitivity::Normal,
        };
        delegate.get().update_config(|config| config.audio.vad = sensitivity);
    });
    let vad = ui::popup(
        mtm,
        &["Low (keeps quiet takes)", "Normal", "High (trims more aggressively)"],
        sensitivity,
        &action,
    );
    ui::sized(&vad, Some(240.0), None);
    keep.keep(action);

    let placeholder = Action::new(mtm, |_sender: &AnyObject| {});
    let silence = ui::toggle(mtm, config.audio.silence_removal, &placeholder);
    let normalize = ui::toggle(mtm, config.audio.normalize, &placeholder);

    let lengths: [(&str, u32); 5] =
        [("1 minute", 60), ("2 minutes", 120), ("5 minutes", 300), ("10 minutes", 600), ("30 minutes", 1800)];
    let length_index =
        lengths.iter().position(|(_, secs)| *secs == config.audio.max_recording_secs).unwrap_or(3);
    let action = popup_changed(mtm, move |index| {
        let secs = lengths.get(index.max(0) as usize).map(|(_, secs)| *secs).unwrap_or(600);
        delegate.get().update_config(|config| config.audio.max_recording_secs = secs);
    });
    let length = ui::popup(mtm, &lengths.map(|(label, _)| label), length_index, &action);
    ui::sized(&length, Some(240.0), None);
    keep.keep(action);

    let (recording, recording_rows) = ui::section_hideable(
        mtm,
        Some("Recording"),
        &[
            ui::row(
                mtm,
                "Silence removal",
                Some("Trims silence from the start and end of a recording before it is transcribed."),
                &[],
                ui::views![silence],
            ),
            ui::row(
                mtm,
                "Sensitivity",
                Some("How aggressively silence is trimmed. Higher trims more but can clip quiet speech."),
                &[],
                ui::views![vad],
            ),
            ui::row(
                mtm,
                "Dynamic normalization",
                Some("Boosts quiet recordings so soft speech is transcribed more reliably."),
                &[],
                ui::views![normalize],
            ),
            ui::row(
                mtm,
                "Maximum length",
                Some("A recording stops automatically after this long."),
                &[],
                ui::views![length],
            ),
        ],
        None,
    );
    recording_rows[1].setHidden(!config.audio.silence_removal);

    let sensitivity_row = recording_rows[1].clone();
    let action = toggled(mtm, move |on| {
        sensitivity_row.setHidden(!on);
        delegate.get().update_config(|config| config.audio.silence_removal = on);
    });
    action.attach(&silence);
    keep.keep(action);
    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.audio.normalize = on);
    });
    action.attach(&normalize);
    keep.keep(action);
    keep.keep(placeholder);

    let selected = match (config.audio.sounds, config.audio.sound_style) {
        (false, _) => 2,
        (true, SoundStyle::Simple) => 0,
        (true, SoundStyle::Classic) => 1,
    };
    let action = Action::new(mtm, move |sender: &AnyObject| {
        let Some(control) = sender.downcast_ref::<objc2_app_kit::NSSegmentedControl>() else {
            return;
        };
        let index = control.selectedSegment();
        delegate.get().update_config(|config| {
            config.audio.sounds = index != 2;
            match index {
                0 => config.audio.sound_style = SoundStyle::Simple,
                1 => config.audio.sound_style = SoundStyle::Classic,
                _ => {}
            }
        });
        delegate.get().preview_sound();
    });
    let style = ui::segmented(mtm, &["Simple", "Classic", "Off"], selected, &action);
    keep.keep(action);

    let action = Action::new(mtm, move |sender: &AnyObject| {
        if let Some(slider) = sender.downcast_ref::<NSSlider>() {
            let volume = slider.doubleValue() as f32;
            delegate.get().update_config(|config| config.audio.sound_volume = volume);
            delegate.get().preview_sound();
        }
    });
    let volume = ui::slider(mtm, 0.0..=1.0, f64::from(config.audio.sound_volume), &action);
    keep.keep(action);
    let quiet = ui::symbol_image("speaker.fill", "Quiet");
    let loud = ui::symbol_image("speaker.wave.3.fill", "Loud");
    let volume_row: Vec<Retained<NSView>> = [quiet, loud]
        .into_iter()
        .map(|image| {
            let view = NSImageView::new(mtm);
            view.setImage(image.as_deref());
            view.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            Retained::into_super(Retained::into_super(view))
        })
        .collect();
    let effects = ui::section(
        mtm,
        Some("Sound effects"),
        &[
            ui::row(
                mtm,
                "Sound effects",
                Some("Plays a short sound when recording starts and stops."),
                &[],
                ui::views![style],
            ),
            ui::row(mtm, "Volume", None, &[], &[&*volume_row[0], &*volume, &*volume_row[1]]),
        ],
        None,
    );

    AudioPage { view: ui::form(mtm, &[input, recording, effects]), device_popup, level }
}

fn permission_row(
    mtm: MainThreadMarker,
    title: &str,
    purpose: &str,
    handler: impl Fn() + 'static,
    keep: &Actions,
) -> (Retained<NSView>, PermissionRow) {
    let status = ui::label(mtm, "");
    let action = clicked(mtm, handler);
    let button = ui::button(mtm, "Open…", &action);
    keep.keep(action);
    let row = ui::row(mtm, title, Some(purpose), &[], ui::views![status, button]);
    (row, PermissionRow { status, button })
}

fn permissions_page(mtm: MainThreadMarker, delegate: Delegate, keep: &Actions) -> PermissionsPage {
    let (microphone_row, microphone) = permission_row(
        mtm,
        "Microphone",
        "Allows dictating text.",
        move || delegate.get().request_microphone(),
        keep,
    );
    let (input_row, input_monitoring) = permission_row(
        mtm,
        "Input Monitoring",
        "Lets the trigger key work in every app.",
        move || delegate.get().open_input_monitoring(),
        keep,
    );
    let (accessibility_row, accessibility) = permission_row(
        mtm,
        "Accessibility",
        "Allows pasting the dictated text into the focused app.",
        move || delegate.get().open_accessibility(),
        keep,
    );
    let access = ui::section(mtm, Some("Access"), &[microphone_row, input_row, accessibility_row], None);

    let action = clicked(mtm, move || delegate.get().restart());
    let restart_button = ui::button(mtm, "Restart tf", &action);
    keep.keep(action);
    let restart = ui::section(
        mtm,
        None,
        &[ui::row(
            mtm,
            "Restart to finish",
            Some("The hotkey permission is granted. Restart tf to activate it."),
            &[],
            ui::views![restart_button],
        )],
        None,
    );
    restart.setHidden(true);

    PermissionsPage {
        view: ui::form(mtm, &[access, restart.clone()]),
        rows: [microphone, input_monitoring, accessibility],
        restart,
    }
}

fn provider_style(id: &str) -> (&'static str, Retained<NSColor>) {
    if id.starts_with("parakeet") {
        ("waveform", NSColor::systemGreenColor())
    } else if id.starts_with("apple") {
        ("applelogo", NSColor::systemGrayColor())
    } else if id.starts_with("qwen") {
        ("sparkles", NSColor::systemPurpleColor())
    } else if id.starts_with("gemma") {
        ("diamond.fill", NSColor::systemBlueColor())
    } else if id.starts_with("llama") {
        ("hare.fill", NSColor::systemOrangeColor())
    } else if id.starts_with("phi") {
        ("square.grid.2x2.fill", NSColor::systemTealColor())
    } else {
        ("shippingbox.fill", NSColor::systemBlueColor())
    }
}

fn installed_model_ids() -> Vec<String> {
    let mut ids: Vec<String> = tf_models::scan(&models_dir()).into_iter().map(|model| model.id).collect();
    ids.extend(tf_models::scan_runtimes(&support_dir()).into_iter().map(|model| model.id));
    ids
}

fn download_in_progress(delegate: Delegate, id: &str) -> bool {
    delegate
        .get()
        .download_view()
        .map(|view| {
            view.id == id
                && !matches!(
                    view.phase,
                    DownloadPhase::Installed | DownloadPhase::Failed(_) | DownloadPhase::Cancelled
                )
        })
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn library_row(
    mtm: MainThreadMarker,
    star: Option<&NSButton>,
    id: &str,
    title: &str,
    tag: Option<&str>,
    tooltip: &str,
    speech: bool,
    trailing: &[&NSView],
) -> Retained<NSView> {
    let content = ui::hstack(mtm, 10.0);
    match star {
        Some(star) => content.addArrangedSubview(star),
        None => {
            let gap = NSView::new(mtm);
            ui::sized(&gap, Some(26.0), Some(26.0));
            content.addArrangedSubview(&gap);
        }
    }
    let (symbol, color) = provider_style(id);
    content.addArrangedSubview(&ui::icon_tile(mtm, symbol, color, 24.0, 6.0));

    let name = ui::label(mtm, title);
    name.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    let heading = ui::hstack(mtm, 6.0);
    heading.addArrangedSubview(&name);
    if let Some(tag) = tag {
        heading.addArrangedSubview(&ui::tag(mtm, tag));
    }
    heading.setContentHuggingPriority_forOrientation(750.0, NSLayoutConstraintOrientation::Horizontal);
    heading.setContentCompressionResistancePriority_forOrientation(
        250.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    content.addArrangedSubview(&heading);

    let spacer = NSView::new(mtm);
    spacer.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    spacer.setContentCompressionResistancePriority_forOrientation(
        1.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    content.addArrangedSubview(&spacer);

    let (badge, kind) =
        if speech { ("waveform", "Speech model") } else { ("text.alignleft", "Language model") };
    content.addArrangedSubview(&ui::type_badge(mtm, badge, kind));
    for view in trailing {
        content.addArrangedSubview(view);
    }
    let row = ui::padded_compact(mtm, &content);
    row.setToolTip(Some(&NSString::from_str(tooltip)));
    row
}

fn model_row(
    mtm: MainThreadMarker,
    delegate: Delegate,
    entry: &CatalogEntry,
    keep: &Actions,
) -> (Retained<NSView>, ModelRow) {
    let speech = entry.kind == "stt";
    let status = ui::value_label(mtm, &entry.size_label());
    status.setAlignment(NSTextAlignment::Right);
    status.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(11.0, 0.0)));
    ui::sized(&status, Some(70.0), None);

    let id = entry.id.clone();
    let action = clicked(mtm, move || {
        if download_in_progress(delegate, &id) {
            delegate.get().cancel_download();
        } else if installed_model_ids().contains(&id) {
            delegate.get().delete_model(&id);
        } else {
            delegate.get().start_model_download(&id);
        }
    });
    let button = ui::symbol_button(mtm, "arrow.down.circle", "Download", &action);
    keep.keep(action);

    let default = speech.then(|| {
        let id = entry.id.clone();
        let action = clicked(mtm, move || {
            if installed_model_ids().contains(&id) {
                delegate.get().set_default_engine(&id);
            }
        });
        let star = ui::symbol_button(mtm, "star", "Use as default", &action);
        ui::set_star(&star, false);
        keep.keep(action);
        star
    });

    let row = library_row(
        mtm,
        default.as_deref(),
        &entry.id,
        &entry.name,
        speech.then(|| entry.language_label()).as_deref(),
        &entry.summary,
        speech,
        ui::views![status, button],
    );
    let handles =
        ModelRow { id: entry.id.clone(), speech, size: entry.size_label(), status, action: button, default };
    (row, handles)
}

fn apply_filter(entries: &[(String, bool, bool, Retained<NSView>)], query: &str, kind: isize) {
    let query = query.trim().to_lowercase();
    for (name, speech, diarization, view) in entries {
        let kind_ok = match kind {
            1 => *speech,
            2 => !*speech && !*diarization,
            3 => *diarization,
            _ => true,
        };
        view.setHidden(!(kind_ok && (query.is_empty() || name.contains(&query))));
    }
}

fn models_page(
    mtm: MainThreadMarker,
    delegate: Delegate,
    config: &Config,
    apple_selected: &Rc<RefCell<Option<String>>>,
    keep: &Actions,
) -> ModelsPage {
    let catalog = Catalog::embedded().unwrap_or(Catalog { version: 1, models: Vec::new() });

    let mut models = Vec::new();
    let mut rows = Vec::new();
    let mut entries: Vec<(String, bool, bool, Retained<NSView>)> = Vec::new();
    let mut push = |name: &str, speech: bool, diarization: bool, row: Retained<NSView>| {
        entries.push((name.to_lowercase(), speech, diarization, row.clone()));
        rows.push(row);
    };
    for entry in catalog.models.iter().filter(|entry| entry.kind == "stt") {
        let (row, handles) = model_row(mtm, delegate, entry, keep);
        push(&entry.name, true, false, row);
        models.push(handles);
    }

    let action = clicked(mtm, move || delegate.get().refresh_settings());
    let apple_popup = ui::popup(mtm, &["Loading…"], 0, &action);
    apple_popup.setControlSize(NSControlSize::Small);
    apple_popup.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    ui::sized(&apple_popup, Some(170.0), None);
    keep.keep(action);

    let selected = Rc::clone(apple_selected);
    let action = clicked(mtm, move || {
        if let Some(locale) = selected.borrow().clone() {
            if delegate.get().download_active() {
                delegate.get().cancel_download();
            } else {
                delegate.get().start_speech_install(&locale);
            }
        }
    });
    let apple_install = ui::symbol_button(mtm, "arrow.down.circle", "Install language", &action);
    keep.keep(action);

    let selected = Rc::clone(apple_selected);
    let action = clicked(mtm, move || {
        if let Some(locale) = selected.borrow().clone() {
            delegate.get().set_default_engine(&format!("apple-speech:{locale}"));
        }
    });
    let apple_default = ui::symbol_button(mtm, "star", "Use as default", &action);
    ui::set_star(&apple_default, false);
    keep.keep(action);
    let apple_row = library_row(
        mtm,
        Some(&apple_default),
        "apple-speech",
        "Apple Speech",
        Some("On-device"),
        "Built-in macOS speech recognition. Pick a language, then install it if needed.",
        true,
        ui::views![apple_popup, apple_install],
    );
    push("apple speech", true, false, apple_row);

    for entry in catalog.models.iter().filter(|entry| entry.kind == "llm") {
        let (row, handles) = model_row(mtm, delegate, entry, keep);
        push(&entry.name, false, false, row);
        models.push(handles);
    }

    for entry in catalog.models.iter().filter(|entry| entry.kind == "diarization") {
        let (row, handles) = model_row(mtm, delegate, entry, keep);
        push(&entry.name, false, true, row);
        models.push(handles);
    }

    let entries = Rc::new(entries);
    let search_ref: Rc<RefCell<Option<Retained<objc2_app_kit::NSSearchField>>>> = Rc::new(RefCell::new(None));
    let kind_ref: Rc<RefCell<Option<Retained<NSPopUpButton>>>> = Rc::new(RefCell::new(None));
    let refilter = {
        let entries = Rc::clone(&entries);
        let search_ref = Rc::clone(&search_ref);
        let kind_ref = Rc::clone(&kind_ref);
        move || {
            let query = search_ref.borrow().as_ref().map(|field| field.stringValue().to_string());
            let kind = kind_ref.borrow().as_ref().map(|popup| popup.indexOfSelectedItem());
            apply_filter(&entries, &query.unwrap_or_default(), kind.unwrap_or(0));
        }
    };
    let action = clicked(mtm, refilter.clone());
    let search = ui::search_field(mtm, "Search models", &action);
    keep.keep(action);
    let action = clicked(mtm, refilter);
    let kind = ui::popup(mtm, &["All models", "Speech", "Language", "Speakers"], 0, &action);
    ui::sized(&kind, Some(150.0), None);
    keep.keep(action);
    *search_ref.borrow_mut() = Some(search.clone());
    *kind_ref.borrow_mut() = Some(kind.clone());

    let filter_bar = ui::hstack(mtm, 10.0);
    filter_bar.addArrangedSubview(&search);
    filter_bar.addArrangedSubview(&kind);
    let filter_bar: Retained<NSView> = Retained::into_super(filter_bar);

    let library = ui::plain_card(mtm, &rows);
    let library: Retained<NSView> = Retained::into_super(library);

    let endpoints = config
        .llm
        .endpoints
        .iter()
        .map(|endpoint| format!("{} — {}", endpoint.name, endpoint.base_url))
        .collect::<Vec<_>>()
        .join("\n");
    let action = clicked(mtm, move || delegate.get().test_endpoint());
    let test = ui::button(mtm, "Test", &action);
    keep.keep(action);
    let endpoints = ui::section(
        mtm,
        Some("Endpoints"),
        &[ui::row(
            mtm,
            "Language model endpoints",
            Some("OpenAI-compatible servers, such as Ollama, that can run cleanup modes."),
            &[&ui::detail(mtm, if endpoints.is_empty() { "No endpoints configured." } else { &endpoints })],
            ui::views![test],
        )],
        None,
    );

    ModelsPage {
        view: ui::form(mtm, &[filter_bar, library, endpoints]),
        models,
        apple_popup,
        apple_install,
        apple_default,
    }
}

fn modes_page(mtm: MainThreadMarker, delegate: Delegate, test_rx: &TestRx, keep: &Actions) -> ModesPage {
    let placeholder = Action::new(mtm, |_sender: &AnyObject| {});
    let ids: Rc<RefCell<Vec<ModeId>>> = Rc::new(RefCell::new(Vec::new()));

    let popup = ui::popup(mtm, &["Loading…"], 0, &placeholder);
    ui::sized(&popup, Some(200.0), None);
    let duplicate = ui::button(mtm, "Duplicate", &placeholder);
    let save = ui::button(mtm, "Save", &placeholder);
    save.setKeyEquivalent(&NSString::from_str("\r"));
    let status = ui::detail(mtm, "");

    let name = ui::field(mtm, "");
    let icon = ui::popup(mtm, &["Speech"], 0, &placeholder);
    ui::sized(&icon, Some(220.0), None);
    let engine = ui::popup(mtm, &["Default"], 0, &placeholder);
    ui::sized(&engine, Some(220.0), None);
    let language = ui::popup(mtm, &["Detect automatically"], 0, &placeholder);
    ui::sized(&language, Some(220.0), None);
    let insertion = ui::popup(mtm, &["Default", "Paste", "Type"], 0, &placeholder);
    ui::sized(&insertion, Some(220.0), None);

    let hotkey_value = Rc::new(Cell::new(TriggerKey::RightOption));
    let hotkey_enabled = Rc::new(Cell::new(false));
    let captured = Rc::clone(&hotkey_value);
    let captured_enabled = Rc::clone(&hotkey_enabled);
    let hotkey =
        ShortcutRecorder::new(mtm, TriggerKey::RightOption, delegate.get().hotkey_paused(), move |key| {
            captured.set(key);
            captured_enabled.set(true);
        });
    let hotkey_check = ui::toggle(mtm, false, &placeholder);
    let vocabulary = ui::toggle(mtm, true, &placeholder);
    let quick_switch = ui::toggle(mtm, true, &placeholder);

    let apps: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let apps_label = ui::detail(mtm, "");
    apps_label.setAlignment(NSTextAlignment::Right);
    ui::sized(&apps_label, Some(220.0), None);
    let add_app = ui::button(mtm, "Add App…", &placeholder);
    let clear_apps = ui::button(mtm, "Clear", &placeholder);

    let processing = ui::popup(
        mtm,
        &["None (raw transcript)", "Apple Intelligence", "Local model", "Endpoint"],
        0,
        &placeholder,
    );
    ui::sized(&processing, Some(220.0), None);
    let model_popup = ui::popup(mtm, &["Loading…"], 0, &placeholder);
    ui::sized(&model_popup, Some(220.0), None);
    let model_field = ui::field(mtm, "");
    model_field.setPlaceholderString(Some(&NSString::from_str("Model name, e.g. llama3.2")));
    let model_hint = ui::detail(mtm, "");
    let temperature = ui::slider(mtm, 0.0..=1.0, 0.3, &placeholder);
    let temperature_value = ui::value_label(mtm, "0.30");
    ui::sized(&temperature_value, Some(40.0), None);
    let timeout = ui::slider(mtm, 1.0..=30.0, 8.0, &placeholder);
    let timeout_value = ui::value_label(mtm, "8 s");
    ui::sized(&timeout_value, Some(40.0), None);
    let (prompt_scroll, prompt) = ui::text_area(mtm, 150.0);
    let reset = ui::button(mtm, "Reset prompt", &placeholder);
    let advanced_button = ui::button(mtm, "Show", &placeholder);

    let test_input = ui::field(mtm, "um so I think we should uh meet on tuesday no wait thursday");
    ui::sized(&test_input, Some(320.0), None);
    let test_output = ui::detail(mtm, "");
    test_output.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    let run = ui::button(mtm, "Test", &placeholder);

    let hotkey_trailing: [&NSView; 2] = [&hotkey, &hotkey_check];
    let apps_trailing: [&NSView; 3] = [&apps_label, &add_app, &clear_apps];
    let model_trailing: [&NSView; 2] = [&model_popup, &model_field];
    let advanced_trailing: [&NSView; 2] = [&reset, &advanced_button];

    let (cleanup_section, cleanup_wrappers) = ui::section_hideable(
        mtm,
        Some("Cleanup"),
        &[
            ui::row(
                mtm,
                "Processing",
                Some("An optional step that cleans up or rewrites the transcript after transcription."),
                &[],
                ui::views![processing],
            ),
            ui::row(
                mtm,
                "Model",
                Some("The language model that runs this prompt."),
                &[&model_hint],
                &model_trailing,
            ),
            ui::block_row(
                mtm,
                "Prompt",
                Some("Instructions the model follows for every dictation."),
                &[],
                &prompt_scroll,
            ),
            ui::row(mtm, "Advanced", Some("Temperature and timeout."), &[], &advanced_trailing),
            ui::row(
                mtm,
                "Temperature",
                Some("Lower values give predictable cleanup; higher values allow more rewording."),
                &[],
                ui::views![temperature, temperature_value],
            ),
            ui::row(
                mtm,
                "Timeout",
                Some("How long to wait for processing before falling back to the raw transcript."),
                &[],
                ui::views![timeout, timeout_value],
            ),
        ],
        None,
    );
    let cleanup = CleanupRows {
        model: cleanup_wrappers[1].clone(),
        prompt: cleanup_wrappers[2].clone(),
        advanced: cleanup_wrappers[3].clone(),
        temperature: cleanup_wrappers[4].clone(),
        timeout: cleanup_wrappers[5].clone(),
    };

    let test_section = ui::section(
        mtm,
        Some("Test"),
        &[
            ui::row(
                mtm,
                "Sample text",
                Some("Text used to try this mode's processing step."),
                &[],
                ui::views![test_input],
            ),
            ui::row(mtm, "Result", None, &[&test_output], ui::views![run]),
        ],
        None,
    );

    let active_button = ui::button(mtm, "Make active", &placeholder);

    let sections = [
        ui::section(
            mtm,
            None,
            &[
                ui::row(
                    mtm,
                    "Name",
                    Some("The name shown in the menu bar and on the pill."),
                    &[],
                    ui::views![name],
                ),
                ui::row(mtm, "Icon", Some("Shown in the menu bar and on the pill."), &[], ui::views![icon]),
                ui::row(
                    mtm,
                    "Active mode",
                    Some("The mode used for dictation unless an app or shortcut picks another."),
                    &[],
                    ui::views![active_button],
                ),
            ],
            None,
        ),
        ui::section(
            mtm,
            Some("Transcription"),
            &[
                ui::row(
                    mtm,
                    "Language",
                    Some("Pick a language if automatic detection gets it wrong."),
                    &[],
                    ui::views![language],
                ),
                ui::row(
                    mtm,
                    "Speech engine",
                    Some(
                        "Which engine transcribes this mode. Default uses the engine chosen on the Models page.",
                    ),
                    &[],
                    ui::views![engine],
                ),
                ui::row(
                    mtm,
                    "Insertion",
                    Some("Overrides how text is inserted for this mode."),
                    &[],
                    ui::views![insertion],
                ),
                ui::row(
                    mtm,
                    "Use vocabulary",
                    Some("Applies your vocabulary terms and replacements to this mode."),
                    &[],
                    ui::views![vocabulary],
                ),
            ],
            None,
        ),
        ui::section(
            mtm,
            Some("Activation"),
            &[
                ui::row(
                    mtm,
                    "Activate for apps",
                    Some("This mode turns on automatically while one of these apps is in front."),
                    &[],
                    &apps_trailing,
                ),
                ui::row(
                    mtm,
                    "Keyboard shortcut",
                    Some("Switch on to start dictation in this mode with its own shortcut."),
                    &[],
                    &hotkey_trailing,
                ),
                ui::row(
                    mtm,
                    "Switch during recording",
                    Some(
                        "Includes this mode when you switch modes with the mode switch key while recording.",
                    ),
                    &[],
                    ui::views![quick_switch],
                ),
            ],
            None,
        ),
        cleanup_section,
        test_section.clone(),
    ];
    let form = ui::form(mtm, &sections);

    let back = ui::symbol_button(mtm, "chevron.left", "Back to modes", &placeholder);
    let title_icon = NSImageView::new(mtm);
    ui::sized(&title_icon, Some(18.0), Some(18.0));
    title_icon.setContentTintColor(Some(&NSColor::labelColor()));
    let title = ui::label(mtm, "");
    title.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
    let spacer = NSView::new(mtm);
    spacer.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    spacer.setContentCompressionResistancePriority_forOrientation(
        1.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    let header = ui::hstack(mtm, 8.0);
    header.addArrangedSubview(&back);
    header.addArrangedSubview(&title_icon);
    header.addArrangedSubview(&title);
    header.addArrangedSubview(&spacer);
    header.addArrangedSubview(&status);
    header.addArrangedSubview(&duplicate);
    header.addArrangedSubview(&save);
    let header = ui::padded(mtm, &header);

    let detail_view = NSView::new(mtm);
    detail_view.setTranslatesAutoresizingMaskIntoConstraints(false);
    detail_view.addSubview(&header);
    detail_view.addSubview(&form);
    form.setTranslatesAutoresizingMaskIntoConstraints(false);
    let constraints = [
        header.topAnchor().constraintEqualToAnchor(&detail_view.topAnchor()),
        header.leadingAnchor().constraintEqualToAnchor(&detail_view.leadingAnchor()),
        header.trailingAnchor().constraintEqualToAnchor(&detail_view.trailingAnchor()),
        form.topAnchor().constraintEqualToAnchor(&header.bottomAnchor()),
        form.leadingAnchor().constraintEqualToAnchor(&detail_view.leadingAnchor()),
        form.trailingAnchor().constraintEqualToAnchor(&detail_view.trailingAnchor()),
        form.bottomAnchor().constraintEqualToAnchor(&detail_view.bottomAnchor()),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    detail_view.setHidden(true);

    let create = ui::button(mtm, "+ Create mode", &placeholder);
    let list_title = ui::label(mtm, "Modes");
    list_title.setFont(Some(&NSFont::boldSystemFontOfSize(15.0)));
    let list_spacer = NSView::new(mtm);
    list_spacer.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    list_spacer.setContentCompressionResistancePriority_forOrientation(
        1.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    let list_header = ui::hstack(mtm, 8.0);
    list_header.addArrangedSubview(&list_title);
    list_header.addArrangedSubview(&ui::InfoButton::new(
        mtm,
        "A mode combines a speech engine, an optional cleanup step and insertion settings. Click one to edit it.",
    ));
    list_header.addArrangedSubview(&list_spacer);
    list_header.addArrangedSubview(&create);
    let list_stack = ui::vstack(mtm, 10.0);
    let list_view: Retained<NSView> = Retained::into_super(ui::form(
        mtm,
        &[Retained::into_super(list_header), Retained::into_super(list_stack.clone())],
    ));
    let container = NSView::new(mtm);
    container.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(&list_view);
    container.addSubview(&detail_view);
    ui::pin_edges(&list_view, &container);
    ui::pin_edges(&detail_view, &container);

    let editor = Rc::new(ModeEditor {
        popup: popup.clone(),
        name,
        icon,
        engine,
        language,
        processing: processing.clone(),
        model_popup,
        model_field,
        model_hint,
        prompt,
        temperature,
        temperature_value,
        timeout,
        timeout_value,
        insertion,
        hotkey,
        hotkey_value,
        hotkey_enabled,
        hotkey_check: hotkey_check.clone(),
        apps,
        apps_label,
        vocabulary,
        quick_switch,
        status,
        test_input,
        test_output,
        ids: Rc::clone(&ids),
        cleanup,
        advanced: Cell::new(false),
        advanced_button: advanced_button.clone(),
        test_section,
        list_view,
        detail_view: detail_view.clone(),
        list_stack,
        list_actions: RefCell::new(Vec::new()),
        title,
        title_icon,
        active_button: active_button.clone(),
    });

    let attach = |action: Action, control: &objc2_app_kit::NSControl| {
        action.attach(control);
        keep.keep(action);
    };

    let for_save = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            if save_mode(delegate, &for_save) {
                close_mode(mtm, delegate, &for_save);
            }
        }),
        &save,
    );

    let for_processing = Rc::clone(&editor);
    attach(popup_changed(mtm, move |_| apply_processing(&for_processing)), &processing);

    let for_advanced = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            for_advanced.advanced.set(!for_advanced.advanced.get());
            apply_processing(&for_advanced);
        }),
        &advanced_button,
    );

    let for_add_app = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            if let Some((_, bundle)) = pick_app(mtm) {
                let mut apps = for_add_app.apps.borrow_mut();
                if !apps.contains(&bundle) {
                    apps.push(bundle);
                }
                drop(apps);
                show_apps(&for_add_app);
            }
        }),
        &add_app,
    );
    let for_clear_apps = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            for_clear_apps.apps.borrow_mut().clear();
            show_apps(&for_clear_apps);
        }),
        &clear_apps,
    );

    let enabled_state = Rc::clone(&editor.hotkey_enabled);
    attach(toggled(mtm, move |on| enabled_state.set(on)), &hotkey_check);

    let slider = editor.temperature.clone();
    let value = editor.temperature_value.clone();
    attach(
        clicked(mtm, move || {
            value.setStringValue(&NSString::from_str(&format!("{:.2}", slider.doubleValue())))
        }),
        &editor.temperature,
    );

    let slider = editor.timeout.clone();
    let value = editor.timeout_value.clone();
    attach(
        clicked(mtm, move || {
            value.setStringValue(&NSString::from_str(&format!("{:.0} s", slider.doubleValue())))
        }),
        &editor.timeout,
    );

    let for_create = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            add_mode(delegate, &for_create, false);
            show_detail(&for_create);
        }),
        &create,
    );
    let for_back = Rc::clone(&editor);
    attach(clicked(mtm, move || close_mode(mtm, delegate, &for_back)), &back);
    let for_active = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            let index = for_active.popup.indexOfSelectedItem().max(0) as usize;
            if let Some(id) = for_active.ids.borrow().get(index).cloned() {
                delegate.get().update_config(|config| config.default_mode_id = id);
            }
        }),
        &active_button,
    );
    let for_duplicate = Rc::clone(&editor);
    attach(clicked(mtm, move || add_mode(delegate, &for_duplicate, true)), &duplicate);

    let for_reset = Rc::clone(&editor);
    attach(
        clicked(mtm, move || {
            let index = for_reset.popup.indexOfSelectedItem();
            let id = for_reset.ids.borrow().get(index.max(0) as usize).cloned();
            if let Some(id) = id
                && let Some(preset) = mode::preset(&id)
                && let Some(refine) = preset.refine
            {
                set_text(&for_reset.prompt, &refine.prompt);
            }
        }),
        &reset,
    );

    let for_run = Rc::clone(&editor);
    let test_rx = Rc::clone(test_rx);
    attach(
        clicked(mtm, move || {
            let _ = save_mode(delegate, &for_run);
            run_mode_test(delegate, &for_run, &test_rx);
        }),
        &run,
    );

    keep.keep(placeholder);
    rebuild_mode_list(mtm, delegate, &editor);
    ModesPage { view: container, editor }
}

fn vocabulary_page(mtm: MainThreadMarker, delegate: Delegate, keep: &Actions) -> VocabularyPage {
    let (terms_scroll, terms) = ui::text_area(mtm, 110.0);
    let (replacements_scroll, replacements) = ui::text_area(mtm, 110.0);
    let status = ui::detail(mtm, "");

    let status_label = status.clone();
    let terms_for_save = terms.clone();
    let replacements_for_save = replacements.clone();
    let action = clicked(mtm, move || {
        match save_vocabulary(delegate.get(), &terms_for_save, &replacements_for_save) {
            Ok(()) => {
                status_label.setStringValue(&NSString::from_str("Saved."));
                ui::set_status_color(&status_label, false);
            }
            Err(e) => {
                status_label.setStringValue(&NSString::from_str(&e));
                ui::set_status_color(&status_label, true);
            }
        }
    });
    let save = ui::button(mtm, "Save", &action);
    keep.keep(action);

    let sections = [
        ui::section(
            mtm,
            Some("Terms"),
            &[ui::block_row(
                mtm,
                "Vocabulary terms",
                Some(
                    "One term per line. Prefix with = for an exact-only term that is never fuzzy-corrected.",
                ),
                &[],
                &terms_scroll,
            )],
            None,
        ),
        ui::section(
            mtm,
            Some("Replacements"),
            &[ui::block_row(
                mtm,
                "Replacements",
                None,
                &[&ui::detail(
                    mtm,
                    "One per line: from => to. Wrap the left side in /slashes/ for a regular expression. \
                     Matching is case-insensitive.",
                )],
                &replacements_scroll,
            )],
            None,
        ),
        ui::section(mtm, None, &[ui::row(mtm, "Save changes", None, &[&status], ui::views![save])], None),
    ];
    VocabularyPage { view: ui::form(mtm, &sections), terms, replacements, status }
}

fn meetings_page(mtm: MainThreadMarker, delegate: Delegate, config: &Config, keep: &Actions) -> MeetingsPage {
    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.meetings.detect = on);
    });
    let detect = ui::toggle(mtm, config.meetings.detect, &action);
    keep.keep(action);

    let action = popup_changed(mtm, move |index| {
        let capture = if index == 1 { CaptureMode::App } else { CaptureMode::All };
        delegate.get().update_config(|config| config.meetings.capture = capture);
    });
    let capture = ui::popup(mtm, &["All system audio", "Only the call app"], 0, &action);
    keep.keep(action);

    let choices = summary_choices(config);
    let labels = choices.iter().map(|(label, _, _)| label.as_str()).collect::<Vec<_>>();
    let action = popup_changed(mtm, move |index| {
        let choices = summary_choices(&delegate.get().config());
        let Some((_, backend, model)) = choices.into_iter().nth(index.max(0) as usize) else {
            return;
        };
        delegate.get().update_config(|config| {
            config.meetings.summary.backend = backend;
            config.meetings.summary.model = model;
        });
    });
    let summary = ui::popup(mtm, &labels, summary_choice_index(&choices, &config.meetings.summary), &action);
    keep.keep(action);

    let summary_endpoint_model = ui::field(mtm, "");
    summary_endpoint_model.setPlaceholderString(Some(&NSString::from_str("Endpoint model, e.g. llama3.2")));
    let (summary_prompt_scroll, summary_prompt) = ui::text_area(mtm, 150.0);
    let save_status = ui::detail(mtm, "");

    let endpoint_field = summary_endpoint_model.clone();
    let prompt_view = summary_prompt.clone();
    let status = save_status.clone();
    let action = clicked(mtm, move || {
        let prompt = get_text(&prompt_view);
        let endpoint_model = endpoint_field.stringValue().to_string();
        delegate.get().update_config(|config| {
            let summary = &mut config.meetings.summary;
            summary.prompt =
                if prompt.trim().is_empty() { mode::MEETING_SUMMARY_PROMPT.to_owned() } else { prompt };
            if matches!(summary.backend, Backend::Endpoint(_)) {
                summary.model = Some(endpoint_model.trim().to_owned()).filter(|model| !model.is_empty());
            }
        });
        status.setStringValue(&NSString::from_str("Saved."));
        ui::set_status_color(&status, false);
    });
    let save_summary = ui::button(mtm, "Save", &action);
    keep.keep(action);

    let action = clicked(mtm, move || {
        delegate.get().update_config(|config| config.meetings.summary = mode::summary_refine());
    });
    let reset_summary = ui::button(mtm, "Reset to default", &action);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.meetings.keep_audio = on);
    });
    let keep_audio = ui::toggle(mtm, config.meetings.keep_audio, &action);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.meetings.echo_cancellation = on);
    });
    let echo = ui::toggle(mtm, config.meetings.echo_cancellation, &action);
    keep.keep(action);

    let action = toggled(mtm, move |on| {
        delegate.get().update_config(|config| config.meetings.identify_speakers = on);
    });
    let identify = ui::toggle(mtm, config.meetings.identify_speakers, &action);
    keep.keep(action);

    let capture_section = ui::section(
        mtm,
        Some("Capture"),
        &[
            ui::row(
                mtm,
                "Detect calls",
                Some("Offer to transcribe when a known meeting app starts using the microphone."),
                &[],
                ui::views![detect],
            ),
            ui::row(
                mtm,
                "Capture",
                Some(
                    "All system audio also includes music and other apps. Only the call app needs the app to be detected first.",
                ),
                &[],
                ui::views![capture],
            ),
            ui::row(
                mtm,
                "Keep meeting audio",
                Some("Two AAC tracks, about 480 KB per minute. Retention follows the History audio setting."),
                &[],
                ui::views![keep_audio],
            ),
            ui::row(
                mtm,
                "Echo cancellation",
                Some(
                    "Uses voice processing when the output looks like speakers. Headphones avoid it entirely.",
                ),
                &[],
                ui::views![echo],
            ),
            ui::row(
                mtm,
                "Identify speakers after a meeting",
                Some(
                    "Splits \"Them\" into individual remote speakers after the meeting, before the summary. \
                     Requires the Speaker identification model in Models.",
                ),
                &[],
                ui::views![identify],
            ),
        ],
        Some(
            "MacOS asks for System Audio Recording permission the first time a meeting starts. You are responsible for telling participants that the call is being transcribed. Speaker identification compares voices on this Mac. Voice data is deleted with the meeting audio.",
        ),
    );

    let summary_section = ui::section(
        mtm,
        Some("Summary"),
        &[
            ui::row(
                mtm,
                "Model",
                Some(
                    "Local models and endpoints are chosen from the Models page. Map-reduce is used for long transcripts.",
                ),
                &[],
                ui::views![summary],
            ),
            ui::row(mtm, "Endpoint model", None, &[], ui::views![summary_endpoint_model]),
            ui::block_row(
                mtm,
                "Prompt",
                Some(
                    "Instructions for summarizing the transcript. Speakers are \"Me\" and \"Them\" or their names.",
                ),
                &[],
                &summary_prompt_scroll,
            ),
            ui::row(mtm, "Save prompt", None, &[&save_status], ui::views![save_summary, reset_summary]),
        ],
        Some("Transcripts and summaries stay on this Mac unless the model is an endpoint you configured."),
    );

    let action = clicked(mtm, move || delegate.get().show_meeting_history());
    let past = ui::button(mtm, "Open", &action);
    keep.keep(action);
    let past_section = ui::section(
        mtm,
        None,
        &[ui::row(
            mtm,
            "Past meetings",
            Some("Chat view, summaries, playback and export of every recorded meeting."),
            &[],
            ui::views![past],
        )],
        None,
    );

    MeetingsPage {
        view: ui::form(mtm, &[past_section, capture_section, summary_section]),
        detect,
        capture,
        summary,
        summary_endpoint_model,
        summary_prompt,
        keep: keep_audio,
        echo,
        identify,
    }
}

fn about_page(mtm: MainThreadMarker, delegate: Delegate, keep: &Actions) -> Retained<NSScrollView> {
    let name = ui::label(mtm, "tf");
    name.setFont(Some(&NSFont::boldSystemFontOfSize(28.0)));
    let tagline = ui::detail(mtm, "Local dictation and meeting transcripts for macOS.");
    let version = ui::detail(mtm, &format!("Version {}", env!("CARGO_PKG_VERSION")));
    let header = ui::vstack(mtm, 4.0);
    for view in ui::views![name, tagline, version] {
        header.addArrangedSubview(view);
    }

    let log = clicked(mtm, || crate::app::open_path(&tf_core::paths::log_path()));
    let log_button = ui::button(mtm, "Open", &log);
    keep.keep(log);
    let data = clicked(mtm, || crate::app::open_path(&tf_core::paths::support_dir()));
    let data_button = ui::button(mtm, "Reveal", &data);
    keep.keep(data);
    let meetings = clicked(mtm, move || delegate.get().show_meeting_history());
    let meetings_button = ui::button(mtm, "Open", &meetings);
    keep.keep(meetings);

    let links = ui::section(
        mtm,
        None,
        &[
            ui::row(
                mtm,
                "Past meetings",
                Some("Browse, search and export recorded meetings."),
                &[],
                ui::views![meetings_button],
            ),
            ui::row(mtm, "Data folder", None, &[], ui::views![data_button]),
            ui::row(mtm, "Log file", None, &[], ui::views![log_button]),
        ],
        None,
    );
    let credits = ui::section(
        mtm,
        Some("Models"),
        &[
            ui::row(
                mtm,
                "Parakeet",
                Some("NVIDIA, CC-BY-4.0. Speech recognition, run through Core ML."),
                &[],
                &[],
            ),
            ui::row(
                mtm,
                "pyannote community-1 and WeSpeaker",
                Some("CC-BY-4.0. Speaker diarization, run through Core ML."),
                &[],
                &[],
            ),
        ],
        Some("Everything runs on this Mac. Audio and transcripts leave it only if you pick a cloud backend."),
    );
    ui::form(mtm, &[Retained::into_super(header), links, credits])
}

fn storage_page(mtm: MainThreadMarker, delegate: Delegate, config: &Config, keep: &Actions) -> StoragePage {
    let action = popup_changed(mtm, move |index| {
        let level = match index {
            0 => HistoryLevel::Off,
            1 => HistoryLevel::Text,
            _ => HistoryLevel::Audio,
        };
        change_history_level(mtm, delegate, level);
    });
    let store = ui::popup(
        mtm,
        &["Don't save", "Save text", "Save text and audio"],
        history_index(config.history.store),
        &action,
    );
    ui::sized(&store, Some(200.0), None);
    keep.keep(action);

    let action = popup_changed(mtm, move |index| {
        let days = retention_value(index);
        delegate.get().update_config(|config| {
            config.history.retention_days = days;
            if let (Some(text), Some(audio)) = (days, config.history.audio_retention_days)
                && audio > text
            {
                config.history.audio_retention_days = Some(text);
            }
        });
        delegate.get().refresh_settings();
    });
    let text = ui::popup(mtm, &RETENTION_LABELS, retention_index(config.history.retention_days), &action);
    keep.keep(action);

    let action = popup_changed(mtm, move |index| {
        let mut days = retention_value(index);
        let text_days = delegate.get().config().history.retention_days;
        if let (Some(text), Some(audio)) = (text_days, days)
            && audio > text
        {
            days = text_days;
        }
        delegate.get().update_config(|config| config.history.audio_retention_days = days);
        delegate.get().refresh_settings();
    });
    let audio =
        ui::popup(mtm, &RETENTION_LABELS, retention_index(config.history.audio_retention_days), &action);
    keep.keep(action);

    let storage = ui::section(
        mtm,
        Some("Storage"),
        &[
            ui::row(mtm, "Save", Some("Choose what is kept after each dictation."), &[], ui::views![store]),
            ui::row(
                mtm,
                "Keep text for",
                Some("Transcripts older than this are deleted automatically."),
                &[],
                ui::views![text],
            ),
            ui::row(
                mtm,
                "Keep audio for",
                Some(
                    "Audio older than this is deleted automatically. Transcripts stay. Audio is AAC mono, about 240 KB per minute.",
                ),
                &[],
                ui::views![audio],
            ),
        ],
        Some("Audio is AAC mono, about 240 KB per minute. Retention runs at launch and every six hours."),
    );

    let usage = ui::value_label(mtm, "");
    let action = clicked(mtm, move || delegate.get().delete_all_history());
    let delete = ui::button(mtm, "Delete All History…", &action);
    keep.keep(action);
    let action = clicked(mtm, crate::app::reveal_audio);
    let reveal = ui::button(mtm, "Show in Finder", &action);
    keep.keep(action);

    let data = ui::section(
        mtm,
        Some("Data"),
        &[
            ui::row(
                mtm,
                "Stored",
                Some("How many dictations and how much audio are on disk."),
                &[],
                ui::views![usage],
            ),
            ui::row(mtm, "Audio files", None, &[], ui::views![reveal]),
            ui::row(
                mtm,
                "Delete history",
                Some("Removes every dictation and its audio."),
                &[],
                ui::views![delete],
            ),
        ],
        None,
    );

    StoragePage { view: ui::form(mtm, &[storage, data]), store, text, audio, usage, delete }
}

fn change_history_level(mtm: MainThreadMarker, delegate: Delegate, level: HistoryLevel) {
    let previous = delegate.get().config().history.store;
    if level == previous {
        return;
    }
    let ask = |message: &str, info: &str, second: &str, first: &str| {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(info));
        alert.addButtonWithTitle(&NSString::from_str(first));
        alert.addButtonWithTitle(&NSString::from_str(second));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        alert.runModal()
    };
    if level == HistoryLevel::Off {
        let response = ask(
            "Stop saving history?",
            "Existing dictations can stay on disk or be deleted now.",
            "Delete All",
            "Keep Existing",
        );
        if response == NSAlertSecondButtonReturn {
            delegate.get().clear_history_now();
        } else if response != NSAlertFirstButtonReturn {
            delegate.get().refresh_settings();
            return;
        }
    } else if previous == HistoryLevel::Audio {
        let response = ask(
            "Stop saving audio?",
            "Existing audio files can be kept, or deleted while the transcripts stay.",
            "Delete Audio",
            "Keep Audio",
        );
        if response == NSAlertSecondButtonReturn {
            delegate.get().clear_audio_now();
        } else if response != NSAlertFirstButtonReturn {
            delegate.get().refresh_settings();
            return;
        }
    }
    delegate.get().update_config(|config| config.history.store = level);
    delegate.get().refresh_settings();
}

const ICONS: [(&str, &str); 14] = [
    ("text.bubble", "Speech"),
    ("wand.and.stars", "Magic wand"),
    ("bubble.left.and.bubble.right", "Chat"),
    ("envelope", "Email"),
    ("list.bullet", "List"),
    ("sparkles", "Sparkles"),
    ("doc.text", "Document"),
    ("chevron.left.forwardslash.chevron.right", "Code"),
    ("globe", "Translate"),
    ("pencil", "Write"),
    ("briefcase", "Work"),
    ("graduationcap", "Study"),
    ("bolt", "Quick"),
    ("heart", "Personal"),
];

const LANGUAGES: [(&str, &str); 18] = [
    ("en", "English"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("cs", "Czech"),
    ("sv", "Swedish"),
    ("da", "Danish"),
    ("no", "Norwegian"),
    ("fi", "Finnish"),
    ("el", "Greek"),
    ("hu", "Hungarian"),
    ("ro", "Romanian"),
];

fn engine_options() -> Vec<(String, Option<String>)> {
    let mut options = vec![("Default".to_owned(), None)];
    if let Ok(catalog) = Catalog::embedded() {
        for entry in catalog.models.iter().filter(|entry| entry.kind == "stt") {
            options.push((entry.name.clone(), Some(entry.id.clone())));
        }
    }
    options
        .push(("Apple Speech (system locale)".to_owned(), Some(format!("apple-speech:{}", system_locale()))));
    options
}

fn engine_options_for(current: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut options = engine_options();
    if let Some(current) = current
        && !options.iter().any(|(_, value)| value.as_deref() == Some(current))
    {
        options.push((current.to_owned(), Some(current.to_owned())));
    }
    options
}

fn icon_options_for(current: &str) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> =
        ICONS.iter().map(|(symbol, label)| ((*label).to_owned(), (*symbol).to_owned())).collect();
    if !options.iter().any(|(_, symbol)| symbol == current) {
        options.push((current.to_owned(), current.to_owned()));
    }
    options
}

fn language_options_for(current: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut options = vec![("Detect automatically".to_owned(), None)];
    options.extend(LANGUAGES.iter().map(|(code, name)| ((*name).to_owned(), Some((*code).to_owned()))));
    if let Some(current) = current
        && !options.iter().any(|(_, value)| value.as_deref() == Some(current))
    {
        options.push((current.to_owned(), Some(current.to_owned())));
    }
    options
}

/// modelle für die meeting-zusammenfassung, als (label, backend, model).
fn summary_choices(config: &Config) -> Vec<(String, Backend, Option<String>)> {
    let mut choices = vec![("Apple Intelligence".to_owned(), Backend::Apple, None)];
    choices.extend(llm_options().into_iter().map(|(name, id)| (name, Backend::Local, Some(id))));
    choices.extend(config.llm.endpoints.iter().map(|endpoint| {
        (format!("Endpoint: {}", endpoint.id), Backend::Endpoint(endpoint.id.clone()), None)
    }));
    choices
}

fn summary_choice_index(choices: &[(String, Backend, Option<String>)], refine: &Refine) -> usize {
    choices
        .iter()
        .position(|(_, backend, model)| {
            *backend == refine.backend && (!refine.backend.is_local() || *model == refine.model)
        })
        .unwrap_or(0)
}

/// installierte sprachmodelle als (anzeigename, catalog-id).
fn llm_options() -> Vec<(String, String)> {
    let installed = installed_model_ids();
    Catalog::embedded()
        .map(|catalog| {
            catalog
                .models
                .iter()
                .filter(|entry| entry.kind == "llm" && installed.contains(&entry.id))
                .map(|entry| (entry.name.clone(), entry.id.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn fill_popup(popup: &NSPopUpButton, labels: &[&str], selected: usize) {
    popup.removeAllItems();
    for label in labels {
        popup.addItemWithTitle(&NSString::from_str(label));
    }
    if !labels.is_empty() {
        popup.selectItemAtIndex(selected.min(labels.len() - 1) as isize);
    }
}

/// füllt das lokale model-popup neu und behält `selected` (eine catalog-id), falls installiert.
fn refresh_model_options(editor: &ModeEditor, selected: Option<&str>) {
    let options = llm_options();
    if options.is_empty() {
        fill_popup(&editor.model_popup, &["No model installed"], 0);
        editor.model_popup.setEnabled(false);
        editor
            .model_hint
            .setStringValue(&NSString::from_str("Download a language model on the Models page."));
    } else {
        let labels: Vec<&str> = options.iter().map(|(name, _)| name.as_str()).collect();
        let index = options.iter().position(|(_, id)| Some(id.as_str()) == selected).unwrap_or(0);
        fill_popup(&editor.model_popup, &labels, index);
        editor.model_popup.setEnabled(true);
        editor.model_hint.setStringValue(&NSString::from_str(""));
    }
}

fn selected_llm(editor: &ModeEditor) -> Option<String> {
    let options = llm_options();
    options.get(editor.model_popup.indexOfSelectedItem().max(0) as usize).map(|(_, id)| id.clone())
}

/// anzeigename der installierten app zu einer bundle-id, sonst der letzte teil der id.
fn app_label(bundle: &str) -> String {
    NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle))
        .and_then(|url| url.path())
        .and_then(|path| {
            Path::new(&path.to_string()).file_stem().and_then(|stem| stem.to_str()).map(str::to_owned)
        })
        .unwrap_or_else(|| bundle.rsplit('.').next().unwrap_or(bundle).to_owned())
}

fn show_apps(editor: &ModeEditor) {
    let apps = editor.apps.borrow();
    let text = if apps.is_empty() {
        "Any app".to_owned()
    } else {
        apps.iter().map(|bundle| app_label(bundle)).collect::<Vec<_>>().join(", ")
    };
    editor.apps_label.setStringValue(&NSString::from_str(&text));
    editor.apps_label.setToolTip(Some(&NSString::from_str(&apps.join("\n"))));
}

fn apply_processing(editor: &ModeEditor) {
    let index = editor.processing.indexOfSelectedItem();
    let cleanup = index > 0;
    let model = index >= 2;
    let advanced = cleanup && editor.advanced.get();
    editor.cleanup.model.setHidden(!model);
    editor.model_popup.setHidden(index != 2);
    editor.model_field.setHidden(index != 3);
    editor.model_hint.setHidden(index != 2);
    editor.cleanup.prompt.setHidden(!cleanup);
    editor.cleanup.advanced.setHidden(!cleanup);
    editor.cleanup.temperature.setHidden(!advanced);
    editor.cleanup.timeout.setHidden(!advanced);
    editor.test_section.setHidden(!cleanup);
    editor.advanced_button.setTitle(&NSString::from_str(if editor.advanced.get() { "Hide" } else { "Show" }));
}

fn show_detail(editor: &ModeEditor) {
    editor.status.setStringValue(&NSString::from_str(""));
    editor.list_view.setHidden(true);
    editor.detail_view.setHidden(false);
}

fn close_mode(mtm: MainThreadMarker, delegate: Delegate, editor: &Rc<ModeEditor>) {
    editor.detail_view.setHidden(true);
    editor.list_view.setHidden(false);
    rebuild_mode_list(mtm, delegate, editor);
}

fn open_mode(editor: &ModeEditor, delegate: Delegate, id: &str) {
    let index = editor.ids.borrow().iter().position(|candidate| candidate == id);
    if let Some(index) = index {
        editor.popup.selectItemAtIndex(index as isize);
        fill_mode_editor(editor, &delegate.get().config(), id);
        show_detail(editor);
    }
}

fn confirm_delete(mtm: MainThreadMarker, name: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("Delete \u{201c}{name}\u{201d}?")));
    alert.setInformativeText(&NSString::from_str("This mode and its settings will be removed."));
    alert.addButtonWithTitle(&NSString::from_str("Delete"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}

fn chip(mtm: MainThreadMarker, symbol: &str, color: Retained<NSColor>, tooltip: &str) -> Retained<NSView> {
    let tile = ui::icon_tile(mtm, symbol, color, 24.0, 6.0);
    tile.setToolTip(Some(&NSString::from_str(tooltip)));
    Retained::into_super(tile)
}

fn mode_row(
    mtm: MainThreadMarker,
    delegate: Delegate,
    editor: &Rc<ModeEditor>,
    config: &Config,
    mode: &Mode,
    actions: &mut Vec<Action>,
) -> Retained<NSView> {
    let content = ui::hstack(mtm, 12.0);
    let icon = NSImageView::new(mtm);
    icon.setImage(ui::symbol_image(&mode.icon, &mode.name).as_deref());
    icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    ui::sized(&icon, Some(20.0), Some(20.0));
    content.addArrangedSubview(&icon);
    let name = ui::label(mtm, &mode.name);
    name.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    content.addArrangedSubview(&name);
    if config.default_mode_id == mode.id {
        let dot = ui::Surface::new(mtm, NSColor::systemGreenColor(), 4.0, false);
        ui::sized(&dot, Some(8.0), Some(8.0));
        dot.setToolTip(Some(&NSString::from_str("Active mode")));
        content.addArrangedSubview(&dot);
    }
    let spacer = NSView::new(mtm);
    spacer.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    spacer.setContentCompressionResistancePriority_forOrientation(
        1.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    content.addArrangedSubview(&spacer);

    let engine = mode.engine.clone().unwrap_or_else(|| config.engine.default.clone());
    let (symbol, color) = provider_style(&engine);
    content.addArrangedSubview(&chip(mtm, symbol, color, "Speech engine"));
    if let Some(refine) = mode.refine() {
        let (symbol, color, tooltip) = match &refine.backend {
            Backend::Apple => ("applelogo", NSColor::systemGrayColor(), "Apple Intelligence".to_owned()),
            Backend::Local => {
                let id = refine.model.clone().unwrap_or_default();
                let (symbol, color) = provider_style(&id);
                (symbol, color, "Local model".to_owned())
            }
            Backend::Endpoint(_) => ("network", NSColor::systemBlueColor(), "Endpoint".to_owned()),
            Backend::Cloud(_) => ("cloud.fill", NSColor::systemCyanColor(), "Cloud".to_owned()),
        };
        content.addArrangedSubview(&chip(mtm, symbol, color, &tooltip));
    }
    let gap = NSView::new(mtm);
    ui::sized(&gap, Some(26.0), Some(1.0));
    content.addArrangedSubview(&gap);

    let padded = ui::padded(mtm, &content);
    ui::sized(&padded, None, Some(52.0));
    let card = ui::Surface::new(mtm, NSColor::quaternarySystemFillColor(), 14.0, false);
    card.wrap(&padded);

    let id = mode.id.clone();
    let for_open = Rc::clone(editor);
    let open = clicked(mtm, move || open_mode(&for_open, delegate, &id));
    let overlay =
        unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(""), None, None, mtm) };
    open.attach(&overlay);
    overlay.setTransparent(true);
    card.addSubview(&overlay);
    ui::pin_edges(&overlay, &card);
    actions.push(open);

    if mode.id != mode::RAW_ID {
        let id = mode.id.clone();
        let name = mode.name.clone();
        let for_delete = Rc::clone(editor);
        let delete = clicked(mtm, move || {
            if !confirm_delete(mtm, &name) {
                return;
            }
            if let Some(index) = for_delete.ids.borrow().iter().position(|candidate| candidate == &id) {
                for_delete.popup.selectItemAtIndex(index as isize);
            }
            delete_mode(delegate, &for_delete);
            rebuild_mode_list(mtm, delegate, &for_delete);
        });
        let button = ui::symbol_button(mtm, "xmark", "Delete mode", &delete);
        card.addSubview(&button);
        let constraints = [
            button.centerYAnchor().constraintEqualToAnchor(&card.centerYAnchor()),
            button.trailingAnchor().constraintEqualToAnchor_constant(&card.trailingAnchor(), -14.0),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        actions.push(delete);
    }
    Retained::into_super(card)
}

fn rebuild_mode_list(mtm: MainThreadMarker, delegate: Delegate, editor: &Rc<ModeEditor>) {
    for view in editor.list_stack.arrangedSubviews().iter() {
        editor.list_stack.removeArrangedSubview(&view);
        view.removeFromSuperview();
    }
    let config = delegate.get().config();
    let mut actions = Vec::new();
    for mode in config.modes() {
        let row = mode_row(mtm, delegate, editor, &config, &mode, &mut actions);
        editor.list_stack.addArrangedSubview(&row);
        ui::fill_width(&editor.list_stack, &row, 0.0);
    }
    *editor.list_actions.borrow_mut() = actions;
}

fn refresh_mode_popup(editor: &ModeEditor, config: &Config, select: &str) {
    let modes = config.modes();
    editor.popup.removeAllItems();
    for mode in &modes {
        editor.popup.addItemWithTitle(&NSString::from_str(&mode.name));
    }
    let ids: Vec<ModeId> = modes.iter().map(|mode| mode.id.clone()).collect();
    let index = ids.iter().position(|id| id == select).unwrap_or(0);
    if !ids.is_empty() {
        editor.popup.selectItemAtIndex(index as isize);
    }
    *editor.ids.borrow_mut() = ids;
    if let Some(id) = editor.ids.borrow().get(index).cloned() {
        fill_mode_editor(editor, config, &id);
    }
}

fn fill_mode_editor(editor: &ModeEditor, config: &Config, id: &str) {
    let Some(mode) = config.mode(id) else {
        return;
    };
    editor.name.setStringValue(&NSString::from_str(&mode.name));
    editor.title.setStringValue(&NSString::from_str(&mode.name));
    editor.title_icon.setImage(ui::symbol_image(&mode.icon, &mode.name).as_deref());
    let active = config.default_mode_id == mode.id;
    editor.active_button.setTitle(&NSString::from_str(if active { "Active" } else { "Make active" }));
    editor.active_button.setEnabled(!active);
    let icons = icon_options_for(&mode.icon);
    editor.icon.removeAllItems();
    for (index, (label, symbol)) in icons.iter().enumerate() {
        editor.icon.addItemWithTitle(&NSString::from_str(label));
        if let Some(item) = editor.icon.itemAtIndex(index as isize) {
            item.setImage(ui::symbol_image(symbol, label).as_deref());
        }
    }
    editor
        .icon
        .selectItemAtIndex(icons.iter().position(|(_, symbol)| symbol == &mode.icon).unwrap_or(0) as isize);

    let languages = language_options_for(mode.language.as_deref());
    let labels: Vec<&str> = languages.iter().map(|(label, _)| label.as_str()).collect();
    fill_popup(
        &editor.language,
        &labels,
        languages.iter().position(|(_, value)| value == &mode.language).unwrap_or(0),
    );

    let options = engine_options_for(mode.engine.as_deref());
    editor.engine.removeAllItems();
    for (label, _) in &options {
        editor.engine.addItemWithTitle(&NSString::from_str(label));
    }
    let engine_index = options.iter().position(|(_, value)| value == &mode.engine).unwrap_or(0);
    editor.engine.selectItemAtIndex(engine_index as isize);

    let processing = match mode.backend() {
        None => 0,
        Some(Backend::Apple) => 1,
        Some(Backend::Local) => 2,
        Some(Backend::Endpoint(_)) | Some(Backend::Cloud(_)) => 3,
    };
    editor.processing.selectItemAtIndex(processing);
    let refine = mode.refine();
    let model = refine.and_then(|r| r.model.as_deref());
    refresh_model_options(editor, model);
    editor.model_field.setStringValue(&NSString::from_str(model.filter(|_| processing == 3).unwrap_or("")));
    editor.temperature.setDoubleValue(refine.map(|r| f64::from(r.temperature)).unwrap_or(0.3));
    editor
        .temperature_value
        .setStringValue(&NSString::from_str(&format!("{:.2}", refine.map(|r| r.temperature).unwrap_or(0.3))));
    editor.timeout.setDoubleValue(refine.map(|r| f64::from(r.timeout_ms) / 1000.0).unwrap_or(8.0));
    editor.timeout_value.setStringValue(&NSString::from_str(&format!(
        "{:.0} s",
        refine.map(|r| r.timeout_ms as f64 / 1000.0).unwrap_or(8.0)
    )));
    set_text(&editor.prompt, refine.map(|r| r.prompt.as_str()).unwrap_or(""));
    editor.insertion.selectItemAtIndex(match mode.insertion {
        None => 0,
        Some(InsertionMethod::Paste) => 1,
        Some(InsertionMethod::Type) => 2,
    });
    let key = mode.hotkey.unwrap_or(TriggerKey::RightOption);
    editor.hotkey_value.set(key);
    editor.hotkey_enabled.set(mode.hotkey.is_some());
    editor.hotkey_check.setState(if mode.hotkey.is_some() {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    editor.hotkey.set_current(key);
    editor.vocabulary.setState(if mode.use_vocabulary {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    editor.quick_switch.setState(state(mode.quick_switch));
    *editor.apps.borrow_mut() = mode.apps.clone();
    show_apps(editor);
    editor.test_output.setStringValue(&NSString::from_str(""));
    apply_processing(editor);
}

fn read_mode(editor: &ModeEditor, config: &Config) -> Option<Mode> {
    let index = editor.popup.indexOfSelectedItem().max(0) as usize;
    let id = editor.ids.borrow().get(index).cloned()?;
    let mut mode = config.mode(&id).unwrap_or_default();
    let current_engine = mode.engine.clone();
    mode.id = id;
    mode.name = editor.name.stringValue().to_string();
    if mode.name.trim().is_empty() {
        mode.name = "Untitled".to_owned();
    }
    let icons = icon_options_for(&mode.icon);
    if let Some((_, symbol)) = icons.get(editor.icon.indexOfSelectedItem().max(0) as usize) {
        mode.icon = symbol.clone();
    }
    let options = engine_options_for(current_engine.as_deref());
    mode.engine =
        options.get(editor.engine.indexOfSelectedItem().max(0) as usize).and_then(|(_, value)| value.clone());
    mode.language = language_options_for(mode.language.as_deref())
        .get(editor.language.indexOfSelectedItem().max(0) as usize)
        .and_then(|(_, value)| value.clone());

    let backend = match editor.processing.indexOfSelectedItem() {
        0 => None,
        1 => Some(Backend::Apple),
        2 => Some(Backend::Local),
        _ => Some(Backend::Endpoint(
            config
                .llm
                .endpoints
                .first()
                .map(|endpoint| endpoint.id.clone())
                .unwrap_or_else(|| "ollama".into()),
        )),
    };
    mode.refine = backend.map(|backend| {
        let model = match backend {
            Backend::Local => selected_llm(editor),
            Backend::Endpoint(_) => {
                Some(editor.model_field.stringValue().to_string()).filter(|model| !model.trim().is_empty())
            }
            _ => None,
        };
        Refine {
            backend,
            model,
            prompt: get_text(&editor.prompt),
            temperature: editor.temperature.doubleValue() as f32,
            timeout_ms: (editor.timeout.doubleValue() * 1000.0).round() as u32,
        }
    });
    mode.insertion = match editor.insertion.indexOfSelectedItem() {
        0 => None,
        1 => Some(InsertionMethod::Paste),
        _ => Some(InsertionMethod::Type),
    };
    mode.hotkey = editor.hotkey_enabled.get().then(|| editor.hotkey_value.get());
    mode.use_vocabulary = editor.vocabulary.state() == NSControlStateValueOn;
    mode.quick_switch = editor.quick_switch.state() == NSControlStateValueOn;
    mode.apps = editor.apps.borrow().clone();
    Some(mode)
}

fn save_mode(delegate: Delegate, editor: &ModeEditor) -> bool {
    let config = delegate.get().config();
    let Some(mode) = read_mode(editor, &config) else {
        return false;
    };
    if let Some(refine) = mode.refine()
        && refine.backend.is_local()
        && refine.model.is_none()
    {
        editor
            .status
            .setStringValue(&NSString::from_str("Download a language model on the Models page first."));
        editor.status.setTextColor(Some(&NSColor::systemRedColor()));
        return false;
    }
    if let Some(key) = mode.hotkey {
        if key == config.trigger.key {
            editor.status.setStringValue(&NSString::from_str(
                "That shortcut is already the global dictation shortcut.",
            ));
            editor.status.setTextColor(Some(&NSColor::systemRedColor()));
            return false;
        }
        if config.trigger.mode_switch_key == Some(key) {
            editor
                .status
                .setStringValue(&NSString::from_str("That shortcut is already the mode switch key."));
            editor.status.setTextColor(Some(&NSColor::systemRedColor()));
            return false;
        }
        if let Some(other) =
            config.modes().iter().find(|candidate| candidate.id != mode.id && candidate.hotkey == Some(key))
        {
            editor.status.setStringValue(&NSString::from_str(&format!(
                "That shortcut is already used by {}.",
                other.name
            )));
            editor.status.setTextColor(Some(&NSColor::systemRedColor()));
            return false;
        }
    }
    let id = mode.id.clone();
    delegate.get().update_config(|config| {
        if config.modes.is_empty() {
            config.modes = config.modes();
        }
        match config.modes.iter_mut().find(|candidate| candidate.id == id) {
            Some(candidate) => *candidate = mode,
            None => config.modes.push(mode),
        }
    });
    editor.status.setStringValue(&NSString::from_str("Saved."));
    editor.status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    true
}

fn add_mode(delegate: Delegate, editor: &ModeEditor, duplicate: bool) {
    let config = delegate.get().config();
    let source = read_mode(editor, &config);
    let mut used: Vec<String> = config.modes().iter().map(|mode| mode.id.clone()).collect();
    let mut index = used.len() + 1;
    while used.contains(&format!("mode-{index}")) {
        index += 1;
    }
    let id = format!("mode-{index}");
    let mut mode = if duplicate { source.unwrap_or_default() } else { Mode::default() };
    mode.id = id.clone();
    mode.name = if duplicate { format!("{} copy", mode.name) } else { "New Mode".to_owned() };
    mode.hotkey = None;
    mode.apps.clear();
    used.push(id.clone());
    delegate.get().update_config(|config| {
        if config.modes.is_empty() {
            config.modes = config.modes();
        }
        config.modes.push(mode);
    });
    let config = delegate.get().config();
    refresh_mode_popup(editor, &config, &id);
}

fn delete_mode(delegate: Delegate, editor: &ModeEditor) {
    let index = editor.popup.indexOfSelectedItem().max(0) as usize;
    let Some(id) = editor.ids.borrow().get(index).cloned() else {
        return;
    };
    if id == mode::RAW_ID {
        return;
    }
    delegate.get().update_config(|config| {
        if config.modes.is_empty() {
            config.modes = config.modes();
        }
        config.modes.retain(|mode| mode.id != id);
        if config.default_mode_id == id {
            config.default_mode_id = mode::RAW_ID.to_owned();
        }
    });
    let config = delegate.get().config();
    refresh_mode_popup(editor, &config, &config.default_mode_id);
}

fn run_mode_test(delegate: Delegate, editor: &ModeEditor, rx: &TestRx) {
    let config = delegate.get().config();
    let Some(mode) = read_mode(editor, &config) else {
        return;
    };
    if mode.refine().is_none() {
        editor.test_output.setStringValue(&NSString::from_str("This mode has no processing step."));
        editor.test_output.setTextColor(Some(&NSColor::systemRedColor()));
        return;
    }
    let input = editor.test_input.stringValue().to_string();
    let (tx, channel) = mpsc::channel();
    *rx.borrow_mut() = Some(channel);
    editor.test_output.setStringValue(&NSString::from_str("Running…"));
    editor.test_output.setTextColor(Some(&NSColor::secondaryLabelColor()));
    std::thread::Builder::new()
        .name("tf-mode-test".into())
        .spawn(move || {
            let result = (|| -> Result<(String, u64), String> {
                let spec = tf_core::session::refiner_spec(&mode, &config, &models_dir())?;
                let mut refiner = tf_llm::build(spec).map_err(|e| e.to_string())?;
                let vocabulary =
                    if mode.use_vocabulary { config.vocabulary.terms_text() } else { Vec::new() };
                let system = tf_core::prompt::system_prompt(&mode, &vocabulary);
                let user = tf_core::prompt::user_prompt(&input);
                let refine = mode.refine().expect("checked above");
                let timeout = Duration::from_millis(u64::from(refine.timeout_ms.max(1000)));
                let request = tf_llm::Request::new(
                    &system,
                    &user,
                    refine.temperature,
                    tf_core::prompt::max_tokens(&input),
                    timeout,
                );
                let started = Instant::now();
                let output = refiner.refine(&request).map_err(|e| e.reason())?;
                let ms = started.elapsed().as_millis() as u64;
                Ok((tf_core::prompt::clean_output(&output, &input), ms))
            })();
            if let Err(e) = result.as_ref() {
                tf_core::log_error!("mode test: {e}");
            }
            let _ = tx.send(result);
        })
        .ok();
}

fn save_vocabulary(app: &AppDelegate, terms: &NSTextView, replacements: &NSTextView) -> Result<(), String> {
    let mut parsed_terms = Vec::new();
    for line in get_text(terms).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (exact, text) = match line.strip_prefix('=') {
            Some(text) => (true, text.trim()),
            None => (false, line),
        };
        if !text.is_empty() {
            parsed_terms.push(Term { text: text.to_owned(), exact });
        }
    }
    let mut parsed_replacements = Vec::new();
    for (number, line) in get_text(replacements).lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((from, to)) = line.split_once("=>") else {
            return Err(format!("Line {}: expected `from => to`.", number + 1));
        };
        let from = from.trim();
        let to = to.trim().replace("\\n", "\n");
        let (kind, from) = if from.len() >= 2 && from.starts_with('/') && from.ends_with('/') {
            (ReplacementKind::Regex, &from[1..from.len() - 1])
        } else {
            (ReplacementKind::Phrase, from)
        };
        let replacement = Replacement { from: from.to_owned(), to, kind, case_sensitive: false };
        validate_replacement(&replacement).map_err(|e| format!("Line {}: {e}", number + 1))?;
        parsed_replacements.push(replacement);
    }
    app.update_config(|config| {
        config.vocabulary.terms = parsed_terms;
        config.vocabulary.replacements = parsed_replacements;
    });
    Ok(())
}

fn system_locale() -> String {
    objc2_foundation::NSLocale::currentLocale().localeIdentifier().to_string().replace('_', "-")
}

const RETENTION_LABELS: [&str; 6] = ["Forever", "1 day", "7 days", "30 days", "90 days", "365 days"];
const RETENTION_VALUES: [Option<u32>; 6] = [None, Some(1), Some(7), Some(30), Some(90), Some(365)];

fn history_index(level: HistoryLevel) -> usize {
    match level {
        HistoryLevel::Off => 0,
        HistoryLevel::Text => 1,
        HistoryLevel::Audio => 2,
    }
}

fn retention_index(days: Option<u32>) -> usize {
    RETENTION_VALUES.iter().position(|value| *value == days).unwrap_or(0)
}

fn retention_value(index: isize) -> Option<u32> {
    RETENTION_VALUES.get(index.max(0) as usize).copied().flatten()
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn launch_at_login() -> bool {
    // SAFETY: der main app service ist ab macos 13 immer verfügbar.
    let service = unsafe { SMAppService::mainAppService() };
    unsafe { service.status() == SMAppServiceStatus::Enabled }
}

fn set_launch_at_login(enable: bool) -> Result<(), String> {
    // SAFETY: der main app service ist ab macos 13 immer verfügbar.
    let service = unsafe { SMAppService::mainAppService() };
    let result = if enable {
        unsafe { service.registerAndReturnError() }
    } else {
        unsafe { service.unregisterAndReturnError() }
    };
    result.map_err(|error| error.localizedDescription().to_string())
}

fn set_text(view: &NSTextView, text: &str) {
    view.setString(&NSString::from_str(text));
}

fn get_text(view: &NSTextView) -> String {
    view.string().to_string()
}

fn pick_app(mtm: MainThreadMarker) -> Option<(PathBuf, String)> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(false);
    panel.setMessage(Some(&NSString::from_str("Choose an application to give its own insertion method.")));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URLs().firstObject()?;
    let path = url.path()?.to_string();
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(&path))?;
    let identifier = bundle.bundleIdentifier()?.to_string();
    Some((PathBuf::from(path), identifier))
}
