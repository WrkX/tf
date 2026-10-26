
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use objc2_app_kit::NSEvent;
use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGPreflightListenEventAccess,
    CGPreflightPostEventAccess, CGRequestListenEventAccess,
};
use tf_core::trigger::{Chord, ESC_KEYCODE, HotkeyEvent, Modifier, TriggerKey, media_function_key};

use crate::app::{MainDispatcher, Message};

pub const TAB_KEYCODE: u16 = 48;

const SYSTEM_DEFINED: CGEventType = CGEventType(14);
const MEDIA_KEY_SUBTYPE: i16 = 8;

pub type Triggers = Arc<RwLock<Vec<(TriggerKey, Option<String>)>>>;
pub type SwitchKey = Arc<RwLock<Option<TriggerKey>>>;

struct Ctx {
    triggers: Triggers,
    switch_key: SwitchKey,
    paused: Arc<AtomicBool>,
    dispatcher: MainDispatcher,
    intercept_esc: Arc<AtomicBool>,
    tap: Mutex<Option<CFRetained<CFMachPort>>>,
}

pub fn spawn(
    triggers: Triggers,
    switch_key: SwitchKey,
    paused: Arc<AtomicBool>,
    dispatcher: MainDispatcher,
    intercept_esc: Arc<AtomicBool>,
) -> Result<(), String> {
    if !CGPreflightListenEventAccess() {
        let _ = CGRequestListenEventAccess();
    }
    // ein aktiver tap ist mit accessibility allein erlaubt, also genügt eine der beiden freigaben.
    if !CGPreflightListenEventAccess() && !CGPreflightPostEventAccess() {
        return Err("Input Monitoring or Accessibility permission is required".into());
    }

    std::thread::Builder::new()
        .name("tf-hotkey".into())
        .spawn(move || run(triggers, switch_key, paused, dispatcher, intercept_esc))
        .map(|_| ())
        .map_err(|e| format!("could not start the hotkey thread: {e}"))
}

fn run(
    triggers: Triggers,
    switch_key: SwitchKey,
    paused: Arc<AtomicBool>,
    dispatcher: MainDispatcher,
    intercept_esc: Arc<AtomicBool>,
) {
    let ctx =
        Box::new(Ctx { triggers, switch_key, paused, dispatcher, intercept_esc, tap: Mutex::new(None) });
    let ctx_ptr = Box::into_raw(ctx);

    let mask = event_mask();
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(callback),
            ctx_ptr.cast(),
        )
    };
    let Some(tap) = tap else {
        // SAFETY: der tap wurde nie gestartet und nur dieser thread hält den pointer.
        drop(unsafe { Box::from_raw(ctx_ptr) });
        dispatcher.send(Message::HotkeyError(
            "Could not install the global hotkey. Grant Input Monitoring and restart tf.".into(),
        ));
        return;
    };

    // SAFETY: ctx_ptr kommt aus einer box oben und lebt bis zum ende des threads.
    if let Ok(mut slot) = unsafe { &*ctx_ptr }.tap.lock() {
        *slot = Some(tap.clone());
    }

    let source = CFMachPort::new_run_loop_source(None, Some(&tap), 0);
    let Some(source) = source else {
        dispatcher.send(Message::HotkeyError("Could not create the hotkey run loop source.".into()));
        return;
    };
    let Some(run_loop) = CFRunLoop::current() else {
        dispatcher.send(Message::HotkeyError("Could not get the hotkey run loop.".into()));
        return;
    };
    run_loop.add_source(Some(&source), unsafe { objc2_core_foundation::kCFRunLoopCommonModes });
    CFRunLoop::run();
}

fn event_mask() -> CGEventMask {
    [CGEventType::KeyDown, CGEventType::KeyUp, CGEventType::FlagsChanged, SYSTEM_DEFINED]
        .iter()
        .fold(0, |mask, event_type| mask | (1u64 << event_type.0))
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    type_: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` zeigt auf das ctx, das dem tap-thread gehört.
    let ctx = unsafe { &*(user_info as *const Ctx) };
    let pass = event.as_ptr();

    if type_ == CGEventType::TapDisabledByTimeout || type_ == CGEventType::TapDisabledByUserInput {
        if let Ok(slot) = ctx.tap.lock()
            && let Some(tap) = slot.as_ref()
        {
            CGEvent::tap_enable(tap, true);
        }
        return pass;
    }

    // das settings-fenster pausiert den tap, während ein neuer shortcut aufgenommen wird.
    if ctx.paused.load(Ordering::Relaxed) {
        return pass;
    }
    let intercept = ctx.intercept_esc.load(Ordering::Relaxed);

    // SAFETY: coregraphics liefert für jeden nicht deaktivierten callback ein gültiges event.
    let event = unsafe { event.as_ref() };
    let mut type_ = type_;
    let mut keycode = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16;
    let flags = CGEvent::flags(Some(event));
    let mut repeat = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventAutorepeat) != 0;

    if type_ == SYSTEM_DEFINED {
        let Some(media) = media_press(event) else {
            return pass;
        };
        keycode = media.keycode;
        repeat = media.repeat;
        type_ = if media.down { CGEventType::KeyDown } else { CGEventType::KeyUp };
    }

    if intercept && type_ == CGEventType::KeyDown && keycode == ESC_KEYCODE {
        if !repeat {
            ctx.dispatcher.send(Message::Hotkey { trigger: None, event: HotkeyEvent::Esc });
        }
        return ptr::null_mut();
    }

    if intercept {
        let configured = ctx.switch_key.read().ok().and_then(|key| *key);
        if let Some(switch) = classify_switch(configured, type_, keycode, flags, repeat) {
            ctx.dispatcher.send(Message::HotkeyModeSwitch { back: switch.back });
            if switch.swallow {
                return ptr::null_mut();
            }
        }
    }

    let triggers = match ctx.triggers.read() {
        Ok(triggers) => triggers.clone(),
        Err(_) => return pass,
    };
    let mut swallowed = false;
    let mut matched = false;
    let mut other = false;
    for (key, mode) in triggers {
        let Some(classified) = classify(key, type_, keycode, flags, repeat) else {
            continue;
        };
        match classified.event {
            HotkeyEvent::OtherKey => other = true,
            _ => {
                ctx.dispatcher.send(Message::Hotkey { trigger: mode, event: classified.event });
                matched = true;
                swallowed |= classified.swallow;
            }
        }
    }
    // eine taste, die zu einem anderen trigger gehört, darf den gerade gestarteten take nicht abbrechen.
    if other && !matched {
        ctx.dispatcher.send(Message::Hotkey { trigger: None, event: HotkeyEvent::OtherKey });
    }
    if swallowed { ptr::null_mut() } else { pass }
}

// TODO: system-events auf macos27 prüfen
/// liest eine medientaste, die stellvertretend für eine funktionstaste gedrückt wird.
fn media_press(event: &CGEvent) -> Option<tf_core::trigger::MediaPress> {
    let event = NSEvent::eventWithCGEvent(event)?;
    if event.subtype().0 != MEDIA_KEY_SUBTYPE {
        return None;
    }
    media_function_key(event.data1() as i64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModeSwitch {
    back: bool,
    swallow: bool,
}

/// erkennt die taste für den moduswechsel während einer aufnahme. ohne konfigurierte taste wechseln tab und shift-tab vor und zurück.
fn classify_switch(
    key: Option<TriggerKey>,
    type_: CGEventType,
    keycode: u16,
    flags: CGEventFlags,
    repeat: bool,
) -> Option<ModeSwitch> {
    match key {
        None => {
            let blocked = CGEventFlags::MaskControl | CGEventFlags::MaskAlternate | CGEventFlags::MaskCommand;
            (type_ == CGEventType::KeyDown && keycode == TAB_KEYCODE && !repeat && !flags.intersects(blocked))
                .then(|| ModeSwitch { back: flags.contains(CGEventFlags::MaskShift), swallow: true })
        }
        Some(key) => {
            if let Some(modifier) = key.modifier() {
                let pressed = type_ == CGEventType::FlagsChanged
                    && key.modifier_keycode() == Some(keycode)
                    && flags.contains(flag_for(modifier));
                return pressed.then_some(ModeSwitch { back: false, swallow: false });
            }
            let chord = key.chord()?;
            let pressed = type_ == CGEventType::KeyDown
                && !repeat
                && keycode == chord.keycode
                && flags.intersection(chord_mask()) == chord_flags(chord);
            pressed.then_some(ModeSwitch { back: false, swallow: true })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Classified {
    event: HotkeyEvent,
    swallow: bool,
}

fn classify(
    key: TriggerKey,
    type_: CGEventType,
    keycode: u16,
    flags: CGEventFlags,
    repeat: bool,
) -> Option<Classified> {
    if let Some(modifier) = key.modifier() {
        if type_ == CGEventType::FlagsChanged && key.modifier_keycode() == Some(keycode) {
            let down = flags.contains(flag_for(modifier));
            return Some(Classified {
                event: if down { HotkeyEvent::Down } else { HotkeyEvent::Up },
                swallow: false,
            });
        }
        if type_ == CGEventType::KeyDown && !repeat {
            return Some(Classified { event: HotkeyEvent::OtherKey, swallow: false });
        }
        return None;
    }

    if let Some(chord) = key.chord() {
        let matches = keycode == chord.keycode && flags.intersection(chord_mask()) == chord_flags(chord);
        if matches {
            return match type_ {
                CGEventType::KeyDown if !repeat => {
                    Some(Classified { event: HotkeyEvent::Down, swallow: true })
                }
                CGEventType::KeyUp => Some(Classified { event: HotkeyEvent::Up, swallow: true }),
                _ => None,
            };
        }
        if type_ == CGEventType::KeyDown && !repeat {
            return Some(Classified { event: HotkeyEvent::OtherKey, swallow: false });
        }
    }
    None
}

const fn chord_mask() -> CGEventFlags {
    CGEventFlags::MaskShift
        .union(CGEventFlags::MaskControl)
        .union(CGEventFlags::MaskAlternate)
        .union(CGEventFlags::MaskCommand)
}

fn flag_for(modifier: Modifier) -> CGEventFlags {
    match modifier {
        Modifier::Shift => CGEventFlags::MaskShift,
        Modifier::Control => CGEventFlags::MaskControl,
        Modifier::Option => CGEventFlags::MaskAlternate,
        Modifier::Command => CGEventFlags::MaskCommand,
    }
}

fn chord_flags(chord: Chord) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    if chord.shift {
        flags |= CGEventFlags::MaskShift;
    }
    if chord.control {
        flags |= CGEventFlags::MaskControl;
    }
    if chord.option {
        flags |= CGEventFlags::MaskAlternate;
    }
    if chord.command {
        flags |= CGEventFlags::MaskCommand;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(event: HotkeyEvent, swallow: bool) -> Option<Classified> {
        Some(Classified { event, swallow })
    }

    #[test]
    fn modifier_down_and_up_are_passed_through() {
        let key = TriggerKey::RightOption;
        assert_eq!(
            classify(key, CGEventType::FlagsChanged, 61, CGEventFlags::MaskAlternate, false),
            result(HotkeyEvent::Down, false)
        );
        assert_eq!(
            classify(key, CGEventType::FlagsChanged, 61, CGEventFlags::empty(), false),
            result(HotkeyEvent::Up, false)
        );
        assert_eq!(classify(key, CGEventType::FlagsChanged, 62, CGEventFlags::MaskControl, false), None);
    }

    #[test]
    fn keys_pressed_with_a_modifier_trigger_are_other_keys() {
        let key = TriggerKey::RightOption;
        assert_eq!(
            classify(key, CGEventType::KeyDown, 14, CGEventFlags::MaskAlternate, false),
            result(HotkeyEvent::OtherKey, false)
        );
        // auto-repeat darf einen take weder neu starten noch unterdrücken.
        assert_eq!(classify(key, CGEventType::KeyDown, 14, CGEventFlags::MaskAlternate, true), None);
    }

    #[test]
    fn chord_is_swallowed_and_requires_an_exact_mask() {
        let key = TriggerKey::Chord { keycode: 2, shift: false, control: true, option: true, command: false };
        let exact = CGEventFlags::MaskControl | CGEventFlags::MaskAlternate;
        assert_eq!(classify(key, CGEventType::KeyDown, 2, exact, false), result(HotkeyEvent::Down, true));
        assert_eq!(classify(key, CGEventType::KeyUp, 2, exact, false), result(HotkeyEvent::Up, true));
        assert_eq!(classify(key, CGEventType::KeyDown, 2, exact, true), None);
        assert_eq!(
            classify(key, CGEventType::KeyDown, 2, CGEventFlags::MaskControl, false),
            result(HotkeyEvent::OtherKey, false)
        );
    }

    #[test]
    fn tab_switches_modes_when_no_key_is_configured() {
        let tab = |flags| classify_switch(None, CGEventType::KeyDown, TAB_KEYCODE, flags, false);
        assert_eq!(tab(CGEventFlags::empty()), Some(ModeSwitch { back: false, swallow: true }));
        assert_eq!(tab(CGEventFlags::MaskShift), Some(ModeSwitch { back: true, swallow: true }));
        assert_eq!(tab(CGEventFlags::MaskCommand), None);
        assert_eq!(
            classify_switch(None, CGEventType::KeyDown, TAB_KEYCODE, CGEventFlags::empty(), true),
            None
        );
    }

    #[test]
    fn modifier_switch_key_reacts_to_press_only_and_is_not_swallowed() {
        let key = Some(TriggerKey::RightOption);
        assert_eq!(
            classify_switch(key, CGEventType::FlagsChanged, 61, CGEventFlags::MaskAlternate, false),
            Some(ModeSwitch { back: false, swallow: false })
        );
        assert_eq!(classify_switch(key, CGEventType::FlagsChanged, 61, CGEventFlags::empty(), false), None);
        assert_eq!(
            classify_switch(key, CGEventType::KeyDown, TAB_KEYCODE, CGEventFlags::empty(), false),
            None
        );
    }

    #[test]
    fn chord_switch_key_needs_an_exact_mask_and_is_swallowed() {
        let key = Some(TriggerKey::Chord {
            keycode: 2,
            shift: false,
            control: true,
            option: false,
            command: false,
        });
        assert_eq!(
            classify_switch(key, CGEventType::KeyDown, 2, CGEventFlags::MaskControl, false),
            Some(ModeSwitch { back: false, swallow: true })
        );
        assert_eq!(classify_switch(key, CGEventType::KeyDown, 2, CGEventFlags::empty(), false), None);
        assert_eq!(classify_switch(key, CGEventType::KeyDown, 2, CGEventFlags::MaskControl, true), None);
    }
}
