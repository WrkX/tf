//! wir pollen alle paar sekunden statt listener zu registrieren: process-objects kommen und gehen häufig, und der poll ist billig.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::coreaudio;

pub const MEETING_APPS: &[(&str, &str)] = &[
    ("us.zoom.xos", "Zoom"),
    ("com.microsoft.teams2", "Teams"),
    ("com.cisco.webexmeetingsapp", "Webex"),
    ("com.apple.FaceTime", "FaceTime"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.hnc.Discord", "Discord"),
];

const BROWSERS: &[(&str, &str)] = &[
    ("com.apple.Safari", "Safari"),
    ("com.google.Chrome", "Chrome"),
    ("company.thebrowser.Browser", "Arc"),
    ("org.mozilla.firefox", "Firefox"),
    ("com.microsoft.edgemac", "Edge"),
];

const PROMPT_AFTER: Duration = Duration::from_secs(5);
const STOP_AFTER: Duration = Duration::from_secs(120);
const SUPPRESS_FOR: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedApp {
    pub bundle_id: String,
    pub name: String,
    pub pid: i32,
}

impl DetectedApp {
    pub fn is_browser(&self) -> bool {
        BROWSERS.iter().any(|(bundle, _)| *bundle == self.bundle_id)
    }
}

pub fn display_name(bundle: &str) -> String {
    MEETING_APPS
        .iter()
        .chain(BROWSERS.iter())
        .find(|(id, _)| *id == bundle)
        .map(|(_, name)| (*name).to_owned())
        .unwrap_or_else(|| bundle.to_owned())
}

// TODO: poll-intervall refac. wenn viel cpu -> auf einen listener umstellen.
pub fn running_input_apps() -> Vec<DetectedApp> {
    coreaudio::process_list()
        .into_iter()
        .filter(|object| coreaudio::process_running_input(*object))
        .filter_map(|object| {
            let bundle_id = coreaudio::process_bundle(object)?;
            let pid = coreaudio::process_pid(object)?;
            Some(DetectedApp { name: display_name(&bundle_id), bundle_id, pid })
        })
        .collect()
}

pub fn candidate(apps: &[DetectedApp]) -> Option<DetectedApp> {
    apps.iter()
        .find(|app| MEETING_APPS.iter().any(|(bundle, _)| *bundle == app.bundle_id))
        .or_else(|| apps.iter().find(|app| app.is_browser()))
        .cloned()
}

pub struct Detector {
    candidate_since: Option<(String, Instant)>,
    suppressed: BTreeMap<String, Instant>,
    quiet_since: Option<Instant>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Self { candidate_since: None, suppressed: BTreeMap::new(), quiet_since: None }
    }

    /// gibt eine app zurück, die das mikrofon mindestens fünf sekunden genutzt hat. der aufrufer entscheidet über den prompt; ein prompt unterdrückt wiederholungen für eine stunde.
    pub fn observe(&mut self, apps: &[DetectedApp], now: Instant) -> Option<DetectedApp> {
        let Some(app) = candidate(apps) else {
            self.candidate_since = None;
            return None;
        };
        if self.suppressed.get(&app.bundle_id).is_some_and(|until| now < *until) {
            return None;
        }
        match self.candidate_since.as_ref() {
            Some((bundle, since)) if *bundle == app.bundle_id => {
                if now.duration_since(*since) >= PROMPT_AFTER {
                    self.candidate_since = None;
                    Some(app)
                } else {
                    None
                }
            }
            _ => {
                self.candidate_since = Some((app.bundle_id.clone(), now));
                None
            }
        }
    }

    pub fn reset_quiet(&mut self) {
        self.quiet_since = None;
    }

    /// unterdrückt prompts für `app` für eine stunde ("Not now").
    pub fn not_now(&mut self, app: &DetectedApp, now: Instant) {
        self.suppressed.insert(app.bundle_id.clone(), now + SUPPRESS_FOR);
        self.candidate_since = None;
    }

    /// true, sobald die aufnehmende app zwei minuten lang kein mikrofon mehr nutzt. stummschalten gibt das mikrofon frei, die schonzeit muss also eine kurze stummschaltung überdauern.
    pub fn should_stop(&mut self, apps: &[DetectedApp], bundle_id: &str, now: Instant) -> bool {
        if apps.iter().any(|app| app.bundle_id == bundle_id) {
            self.quiet_since = None;
            return false;
        }
        match self.quiet_since {
            Some(since) => now.duration_since(since) >= STOP_AFTER,
            None => {
                self.quiet_since = Some(now);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(bundle: &str, pid: i32) -> DetectedApp {
        DetectedApp { bundle_id: bundle.to_owned(), name: display_name(bundle), pid }
    }

    #[test]
    fn meeting_apps_beat_browsers() {
        let apps = [app("com.google.Chrome", 1), app("us.zoom.xos", 2)];
        assert_eq!(candidate(&apps).unwrap().bundle_id, "us.zoom.xos");
    }

    #[test]
    fn unknown_processes_are_ignored() {
        assert!(candidate(&[app("com.example.tool", 3)]).is_none());
    }

    #[test]
    fn prompt_needs_five_seconds() {
        let mut detector = Detector::new();
        let now = Instant::now();
        let apps = [app("us.zoom.xos", 1)];
        assert!(detector.observe(&apps, now).is_none());
        assert!(detector.observe(&apps, now + Duration::from_secs(4)).is_none());
        assert!(detector.observe(&apps, now + Duration::from_secs(6)).is_some());
    }

    #[test]
    fn not_now_suppresses_for_an_hour() {
        let mut detector = Detector::new();
        let now = Instant::now();
        let apps = [app("com.microsoft.teams2", 2)];
        assert!(detector.observe(&apps, now).is_none());
        let app = detector.observe(&apps, now + Duration::from_secs(6)).expect("first candidate");
        assert!(app.bundle_id.contains("teams2"));
        detector.not_now(&app, now + Duration::from_secs(6));
        assert!(detector.observe(&apps, now + Duration::from_secs(12)).is_none());
        assert!(detector.observe(&apps, now + Duration::from_secs(3_700)).is_none());
        assert!(detector.observe(&apps, now + Duration::from_secs(3_706)).is_some());
    }

    #[test]
    fn stop_needs_two_quiet_minutes() {
        let mut detector = Detector::new();
        let now = Instant::now();
        assert!(!detector.should_stop(&[], "us.zoom.xos", now));
        assert!(!detector.should_stop(&[], "us.zoom.xos", now + Duration::from_secs(100)));
        assert!(detector.should_stop(&[], "us.zoom.xos", now + Duration::from_secs(121)));
        assert!(!detector.should_stop(
            &[app("us.zoom.xos", 1)],
            "us.zoom.xos",
            now + Duration::from_secs(130)
        ));
    }
}
