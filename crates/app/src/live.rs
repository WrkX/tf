//! das panel wird nie key, tippen in der call-app bleibt dadurch unberührt.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFont, NSLayoutConstraint, NSPanel, NSScrollView, NSStackView,
    NSTextField, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

use tf_meeting::{Speaker, summary};

use crate::app::AppDelegate;
use crate::chat;
use crate::ui::{self, Action};

#[derive(Clone, Copy)]
struct Delegate(*const AppDelegate);

impl Delegate {
    fn get(self) -> &'static AppDelegate {
        // SAFETY: der app-delegate lebt für die gesamte laufzeit der app.
        unsafe { &*self.0 }
    }
}

pub struct LiveTranscript {
    panel: Retained<NSPanel>,
    title: Retained<NSTextField>,
    status: Retained<NSTextField>,
    chat: Retained<NSStackView>,
    stop: Retained<NSButton>,
    followed: Cell<bool>,
    mtm: MainThreadMarker,
    #[allow(dead_code)]
    actions: RefCell<Vec<Action>>,
}

impl LiveTranscript {
    pub fn new(mtm: MainThreadMarker, delegate: &AppDelegate) -> Self {
        let delegate = Delegate(delegate as *const AppDelegate);
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::new(140.0, 140.0), NSSize::new(620.0, 420.0)),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&NSString::from_str("Live Transcript"));
        panel.setLevel(objc2_app_kit::NSFloatingWindowLevel);
        panel.setHidesOnDeactivate(false);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        let title = ui::label(mtm, "Meeting");
        title.setFont(Some(&NSFont::boldSystemFontOfSize(15.0)));
        let status = ui::detail(mtm, "00:00");

        let actions = RefCell::new(Vec::new());
        let action = Action::new(mtm, move |_sender: &AnyObject| delegate.get().stop_meeting());
        let stop = ui::button(mtm, "Stop", &action);
        actions.borrow_mut().push(action);
        let action = Action::new(mtm, move |_sender: &AnyObject| delegate.get().toggle_live_transcript());
        let hide = ui::button(mtm, "Hide", &action);
        actions.borrow_mut().push(action);

        let chat = chat::conversation(mtm);
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setDocumentView(Some(&chat));
        let clip = scroll.contentView();
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            chat.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
            chat.leadingAnchor().constraintEqualToAnchor(&clip.leadingAnchor()),
            chat.trailingAnchor().constraintEqualToAnchor(&clip.trailingAnchor()),
        ]));

        let content = NSView::new(mtm);
        panel.setContentView(Some(&content));
        for part in [&title as &NSView, &status, &stop, &hide, &scroll] {
            part.setTranslatesAutoresizingMaskIntoConstraints(false);
            content.addSubview(part);
        }
        let constraints = [
            title.topAnchor().constraintEqualToAnchor_constant(&content.topAnchor(), 14.0),
            title.leadingAnchor().constraintEqualToAnchor_constant(&content.leadingAnchor(), 18.0),
            status.leadingAnchor().constraintEqualToAnchor_constant(&title.trailingAnchor(), 10.0),
            status.centerYAnchor().constraintEqualToAnchor(&title.centerYAnchor()),
            hide.trailingAnchor().constraintEqualToAnchor_constant(&content.trailingAnchor(), -18.0),
            hide.centerYAnchor().constraintEqualToAnchor(&title.centerYAnchor()),
            stop.trailingAnchor().constraintEqualToAnchor_constant(&hide.leadingAnchor(), -8.0),
            stop.centerYAnchor().constraintEqualToAnchor(&title.centerYAnchor()),
            scroll.topAnchor().constraintEqualToAnchor_constant(&title.bottomAnchor(), 12.0),
            scroll.leadingAnchor().constraintEqualToAnchor_constant(&content.leadingAnchor(), 12.0),
            scroll.trailingAnchor().constraintEqualToAnchor_constant(&content.trailingAnchor(), -12.0),
            scroll.bottomAnchor().constraintEqualToAnchor_constant(&content.bottomAnchor(), -12.0),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));

        Self { panel, title, status, chat, stop, followed: Cell::new(true), mtm, actions }
    }

    pub fn is_visible(&self) -> bool {
        self.panel.isVisible()
    }

    pub fn show(&self, title: &str) {
        self.title.setStringValue(&NSString::from_str(title));
        self.panel.orderFront(None);
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    pub fn stop_enabled(&self, enabled: bool) {
        self.stop.setEnabled(enabled);
    }

    pub fn clear(&self) {
        for view in self.chat.arrangedSubviews().iter() {
            self.chat.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        self.followed.set(true);
    }

    pub fn set_elapsed(&self, seconds: u64) {
        self.status.setStringValue(&NSString::from_str(&format!("{:02}:{:02}", seconds / 60, seconds % 60)));
    }

    pub fn append(&self, speaker: Speaker, start_ms: u64, text: &str) {
        let is_me = speaker == Speaker::Me;
        let time = summary::clock(start_ms);
        let row = chat::row(
            self.mtm,
            chat::Bubble {
                is_me,
                name: speaker.label(),
                name_color: NSColor::secondaryLabelColor(),
                time: &time,
                text: text.trim(),
                continued: false,
                max_width: 360.0,
                controls: Vec::new(),
            },
        );
        chat::push(&self.chat, &row);
        if self.followed.get() {
            self.chat.layoutSubtreeIfNeeded();
            self.chat.scrollRectToVisible(row.frame());
        }
    }
}
