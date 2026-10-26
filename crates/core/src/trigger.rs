use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const ESC_KEYCODE: u16 = 53;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerKey {
    #[default]
    RightOption,
    RightCommand,
    RightControl,
    RightShift,
    Chord {
        keycode: u16,
        shift: bool,
        control: bool,
        option: bool,
        command: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Control,
    Option,
    Command,
}

impl TriggerKey {
    const CHORD: u64 = 1 << 63;

    /// packt den trigger in ein atomares wort, damit der event-tap-callback ihn ohne lock lesen kann.
    pub fn encode(self) -> u64 {
        match self {
            Self::RightOption => 1,
            Self::RightCommand => 2,
            Self::RightControl => 3,
            Self::RightShift => 4,
            Self::Chord { keycode, shift, control, option, command } => {
                Self::CHORD
                    | u64::from(keycode)
                    | ((shift as u64) << 16)
                    | ((control as u64) << 17)
                    | ((option as u64) << 18)
                    | ((command as u64) << 19)
            }
        }
    }

    pub fn decode(bits: u64) -> Self {
        if bits & Self::CHORD != 0 {
            return Self::Chord {
                keycode: (bits & 0xffff) as u16,
                shift: bits & (1 << 16) != 0,
                control: bits & (1 << 17) != 0,
                option: bits & (1 << 18) != 0,
                command: bits & (1 << 19) != 0,
            };
        }
        match bits {
            1 => Self::RightOption,
            2 => Self::RightCommand,
            3 => Self::RightControl,
            4 => Self::RightShift,
            _ => Self::RightOption,
        }
    }

    pub fn modifier_keycode(self) -> Option<u16> {
        Some(match self {
            Self::RightOption => 61,
            Self::RightCommand => 54,
            Self::RightControl => 62,
            Self::RightShift => 60,
            Self::Chord { .. } => return None,
        })
    }

    pub fn modifier(self) -> Option<Modifier> {
        Some(match self {
            Self::RightOption => Modifier::Option,
            Self::RightCommand => Modifier::Command,
            Self::RightControl => Modifier::Control,
            Self::RightShift => Modifier::Shift,
            Self::Chord { .. } => return None,
        })
    }

    pub fn chord(self) -> Option<Chord> {
        match self {
            Self::Chord { keycode, shift, control, option, command } => {
                Some(Chord { keycode, shift, control, option, command })
            }
            _ => None,
        }
    }

    pub fn display(self) -> String {
        match self {
            Self::RightOption => "Right ⌥".into(),
            Self::RightCommand => "Right ⌘".into(),
            Self::RightControl => "Right ⌃".into(),
            Self::RightShift => "Right ⇧".into(),
            Self::Chord { keycode, shift, control, option, command } => {
                let mut s = String::new();
                if control {
                    s.push('⌃');
                }
                if option {
                    s.push('⌥');
                }
                if shift {
                    s.push('⇧');
                }
                if command {
                    s.push('⌘');
                }
                s.push_str(key_name(keycode));
                s
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub keycode: u16,
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

impl Chord {
    pub fn modifiers(self) -> [bool; 4] {
        [self.shift, self.control, self.option, self.command]
    }
}

const FUNCTION_KEYS: [(u16, &str); 20] = [
    (122, "F1"),
    (120, "F2"),
    (99, "F3"),
    (118, "F4"),
    (96, "F5"),
    (97, "F6"),
    (98, "F7"),
    (100, "F8"),
    (101, "F9"),
    (109, "F10"),
    (103, "F11"),
    (111, "F12"),
    (105, "F13"),
    (107, "F14"),
    (113, "F15"),
    (106, "F16"),
    (64, "F17"),
    (79, "F18"),
    (80, "F19"),
    (90, "F20"),
];

/// funktionstasten können allein als trigger dienen, ohne modifier.
pub fn is_function_key(keycode: u16) -> bool {
    FUNCTION_KEYS.iter().any(|(code, _)| *code == keycode)
}

/// die funktionstaste hinter `data1` eines system-media-events. mit "Use F1, F2, etc. keys as standard function keys" aus schickt macos diese als system-events statt als tastendrücke.
pub fn media_function_key(data1: i64) -> Option<MediaPress> {
    let name = match (data1 >> 16) & 0xffff {
        3 => "F1",
        2 => "F2",
        22 => "F5",
        21 => "F6",
        18 | 20 => "F7",
        16 => "F8",
        17 | 19 => "F9",
        7 => "F10",
        1 => "F11",
        0 => "F12",
        _ => return None,
    };
    let keycode = FUNCTION_KEYS.iter().find(|(_, key)| *key == name).map(|(code, _)| *code)?;
    Some(MediaPress { keycode, down: (data1 >> 8) & 0xff == 0x0a, repeat: data1 & 1 != 0 })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaPress {
    pub keycode: u16,
    pub down: bool,
    pub repeat: bool,
}

fn key_name(keycode: u16) -> &'static str {
    if let Some((_, name)) = FUNCTION_KEYS.iter().find(|(code, _)| *code == keycode) {
        return name;
    }
    const NAMES: [&str; 50] = [
        "A", "S", "D", "F", "H", "G", "Z", "X", "C", "V", "B", "Q", "W", "E", "R", "Y", "T", "1", "2", "3",
        "4", "6", "5", "=", "9", "7", "-", "8", "0", "]", "O", "U", "[", "I", "P", "L", "J", "'", "K", ";",
        "\\", ",", "/", "N", "M", ".", "\t", "Space", "`", "\u{8}",
    ];
    NAMES.get(keycode as usize).copied().unwrap_or("?")
}

/// die eingaben, die die trigger-logik verarbeitet, mit monotonen zeitstempeln.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Down(Duration),
    Up(Duration),
    OtherKey(Duration),
    Esc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down,
    Up,
    OtherKey,
    Esc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Cancel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Style {
    Hold,
    Toggle,
    #[default]
    Hybrid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Pressed { at: Duration },
    Suppressed,
    Latched,
    Draining,
}

/// hold/toggle/hybrid-entscheidung. reine funktion, deshalb ohne event-tap testbar.
#[derive(Clone, Debug)]
pub struct Machine {
    style: Style,
    threshold: Duration,
    state: State,
}

impl Machine {
    pub fn new(style: Style, hold_threshold_ms: u32) -> Self {
        Self { style, threshold: Duration::from_millis(u64::from(hold_threshold_ms)), state: State::Idle }
    }

    pub fn handle(&mut self, input: Input) -> Option<Action> {
        use Input::*;
        use State::*;

        match (self.state, input) {
            (Idle, Down(t)) => {
                self.state = Pressed { at: t };
                Some(Action::Start)
            }
            (Pressed { at }, Up(t)) => {
                let held = t.saturating_sub(at);
                if held < self.threshold {
                    match self.style {
                        Style::Hold => {
                            self.state = Idle;
                            Some(Action::Cancel)
                        }
                        Style::Toggle | Style::Hybrid => {
                            self.state = Latched;
                            None
                        }
                    }
                } else {
                    match self.style {
                        Style::Toggle => {
                            self.state = Latched;
                            None
                        }
                        Style::Hold | Style::Hybrid => {
                            self.state = Idle;
                            Some(Action::Stop)
                        }
                    }
                }
            }
            (Pressed { at }, OtherKey(t)) => {
                if t.saturating_sub(at) < self.threshold {
                    self.state = Suppressed;
                    Some(Action::Cancel)
                } else {
                    None
                }
            }
            (Suppressed, Up(_)) => {
                self.state = Idle;
                None
            }
            (Latched, Down(_)) => {
                self.state = Draining;
                Some(Action::Stop)
            }
            (Draining, Up(_)) => {
                self.state = Idle;
                None
            }
            (Pressed { .. } | Latched | Draining | Suppressed, Esc) => {
                self.state = Idle;
                Some(Action::Cancel)
            }
            _ => None,
        }
    }

    /// setzt die zustandsmaschine ohne aktion auf `Idle`. genutzt, wenn ein take vor dem aufnahmestart scheitert.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn hold_stops_on_release() {
        let mut m = Machine::new(Style::Hold, 300);
        assert_eq!(m.handle(Input::Down(ms(0))), Some(Action::Start));
        assert_eq!(m.handle(Input::Up(ms(900))), Some(Action::Stop));
        assert_eq!(m.handle(Input::Up(ms(1000))), None);
    }

    #[test]
    fn hold_short_tap_cancels() {
        let mut m = Machine::new(Style::Hold, 300);
        m.handle(Input::Down(ms(0)));
        assert_eq!(m.handle(Input::Up(ms(100))), Some(Action::Cancel));
    }

    #[test]
    fn toggle_latches_on_tap_and_stops_on_second_press() {
        let mut m = Machine::new(Style::Toggle, 300);
        assert_eq!(m.handle(Input::Down(ms(0))), Some(Action::Start));
        assert_eq!(m.handle(Input::Up(ms(80))), None);
        assert_eq!(m.handle(Input::Down(ms(2000))), Some(Action::Stop));
        assert_eq!(m.handle(Input::Up(ms(2100))), None);
        assert_eq!(m.handle(Input::Down(ms(3000))), Some(Action::Start));
    }

    #[test]
    fn toggle_hold_stays_latched_after_release() {
        let mut m = Machine::new(Style::Toggle, 300);
        m.handle(Input::Down(ms(0)));
        assert_eq!(m.handle(Input::Up(ms(900))), None);
        assert_eq!(m.handle(Input::Down(ms(1500))), Some(Action::Stop));
    }

    #[test]
    fn hybrid_tap_latches_and_hold_stops() {
        let mut m = Machine::new(Style::Hybrid, 300);
        assert_eq!(m.handle(Input::Down(ms(0))), Some(Action::Start));
        assert_eq!(m.handle(Input::Up(ms(100))), None);
        assert_eq!(m.handle(Input::Down(ms(1000))), Some(Action::Stop));
        m.handle(Input::Up(ms(1050)));

        assert_eq!(m.handle(Input::Down(ms(2000))), Some(Action::Start));
        assert_eq!(m.handle(Input::Up(ms(2900))), Some(Action::Stop));
    }

    #[test]
    fn other_key_within_threshold_suppresses() {
        let mut m = Machine::new(Style::Hybrid, 300);
        m.handle(Input::Down(ms(0)));
        assert_eq!(m.handle(Input::OtherKey(ms(120))), Some(Action::Cancel));
        assert_eq!(m.handle(Input::Up(ms(150))), None);
        assert_eq!(m.handle(Input::Down(ms(500))), Some(Action::Start));
    }

    #[test]
    fn other_key_after_threshold_is_ignored() {
        let mut m = Machine::new(Style::Hybrid, 300);
        m.handle(Input::Down(ms(0)));
        assert_eq!(m.handle(Input::OtherKey(ms(500))), None);
        assert_eq!(m.handle(Input::Up(ms(900))), Some(Action::Stop));
    }

    #[test]
    fn esc_cancels_any_recording_state() {
        let mut m = Machine::new(Style::Hold, 300);
        m.handle(Input::Down(ms(0)));
        assert_eq!(m.handle(Input::Esc), Some(Action::Cancel));
        assert_eq!(m.handle(Input::Down(ms(600))), Some(Action::Start));

        for style in [Style::Toggle, Style::Hybrid] {
            let mut m = Machine::new(style, 300);
            m.handle(Input::Down(ms(0)));
            m.handle(Input::Up(ms(100)));
            assert_eq!(m.handle(Input::Esc), Some(Action::Cancel), "{style:?}");
            assert_eq!(m.handle(Input::Down(ms(600))), Some(Action::Start), "{style:?}");
        }
    }

    #[test]
    fn esc_in_idle_does_nothing() {
        let mut m = Machine::new(Style::Hybrid, 300);
        assert_eq!(m.handle(Input::Esc), None);
    }

    #[test]
    fn depletion_ignores_release_after_stop() {
        let mut m = Machine::new(Style::Hold, 300);
        m.handle(Input::Down(ms(0)));
        assert_eq!(m.handle(Input::Up(ms(800))), Some(Action::Stop));
        assert_eq!(m.handle(Input::Down(ms(1000))), Some(Action::Start));
    }

    #[test]
    fn chord_display() {
        let key = TriggerKey::Chord { keycode: 2, shift: false, control: true, option: true, command: false };
        assert_eq!(key.display(), "⌃⌥D");
    }

    #[test]
    fn encode_decode_round_trip() {
        for key in [
            TriggerKey::RightOption,
            TriggerKey::RightCommand,
            TriggerKey::RightControl,
            TriggerKey::RightShift,
            TriggerKey::Chord { keycode: 2, shift: true, control: true, option: true, command: false },
            TriggerKey::Chord { keycode: 49, shift: false, control: false, option: true, command: true },
        ] {
            assert_eq!(TriggerKey::decode(key.encode()), key);
        }
    }
}
