//! die liste ist eine einfache zellenbasierte `NSTableView`; die zeilen kommen aus einer schreibgeschützten sqlite-verbindung, damit die schreibverbindung des workers die ui nie blockiert.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSButton, NSColor, NSFont, NSLayoutConstraint, NSLineBreakMode, NSPopUpButton,
    NSScrollView, NSSearchField, NSSegmentedControl, NSStackView, NSTableColumn, NSTableView,
    NSTableViewColumnAutoresizingStyle, NSTableViewDataSource, NSTextField, NSTextView, NSView,
};
use objc2_foundation::{NSArray, NSDate, NSDateFormatter, NSIndexSet, NSPoint, NSRect, NSSize, NSString};

use tf_core::paths::history_path;
use tf_meeting::summary;
use tf_store::{MeetingId, MeetingSpeaker, MeetingSummary, Query, Segment, Store, TakeId, TakeSummary};

use crate::app::AppDelegate;
use crate::chat;
use crate::ui::{self, Action, Surface};

const LIST_WIDTH: f64 = 300.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListMode {
    Dictations,
    Meetings,
}

#[derive(Clone, Copy)]
struct Delegate(*const AppDelegate);

impl Delegate {
    fn get(self) -> &'static AppDelegate {
        // SAFETY: der app-delegate lebt für die gesamte laufzeit der app.
        unsafe { &*self.0 }
    }
}

pub struct SourceIvars {
    rows: RefCell<Vec<String>>,
}

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfHistorySource"]
    #[ivars = SourceIvars]
    pub struct HistorySource;

    unsafe impl NSObjectProtocol for HistorySource {}

    unsafe impl NSTableViewDataSource for HistorySource {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().rows.borrow().len() as isize
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<NSString>> {
            let rows = self.ivars().rows.borrow();
            rows.get(row as usize).map(|text| NSString::from_str(text))
        }
    }
);

impl HistorySource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SourceIvars { rows: RefCell::new(Vec::new()) });
        unsafe { msg_send![super(this), init] }
    }

    fn set_rows(&self, rows: Vec<String>) {
        *self.ivars().rows.borrow_mut() = rows;
    }

    fn append_rows(&self, rows: Vec<String>) {
        self.ivars().rows.borrow_mut().extend(rows);
    }
}

pub struct History {
    view: Retained<NSView>,
    mtm: MainThreadMarker,
    source: Retained<HistorySource>,
    table: Retained<NSTableView>,
    search: Retained<NSSearchField>,
    switcher: Retained<NSSegmentedControl>,
    detail: Retained<NSTextView>,
    detail_card: Retained<NSView>,
    chat_card: Retained<NSView>,
    chat_stack: Retained<NSStackView>,
    chat_toggle: Retained<NSSegmentedControl>,
    meta: Retained<NSTextField>,
    play: Retained<NSButton>,
    copy: Retained<NSButton>,
    delete: Retained<NSButton>,
    rerun: Retained<NSPopUpButton>,
    regen: Retained<NSButton>,
    retry: Retained<NSButton>,
    more: Retained<NSButton>,
    #[allow(dead_code)]
    actions: RefCell<Vec<Action>>,
    delegate: Delegate,
    mode: Cell<ListMode>,
    summaries: RefCell<Vec<TakeSummary>>,
    meetings: RefCell<Vec<MeetingSummary>>,
    selected: Cell<Option<TakeId>>,
    selected_meeting: Cell<Option<MeetingId>>,
    before: Cell<Option<i64>>,
    exhausted: Cell<bool>,
    meeting_before: Cell<Option<i64>>,
    meeting_exhausted: Cell<bool>,
    chat_preference: Cell<Option<bool>>,
    diarization_status: Cell<Option<(MeetingId, f32)>>,
}

impl History {
    pub fn new(mtm: MainThreadMarker, delegate: &AppDelegate) -> Self {
        let delegate = Delegate(delegate as *const AppDelegate);
        let actions = RefCell::new(Vec::new());
        let source = HistorySource::new(mtm);
        let keep = |action: Action| actions.borrow_mut().push(action);

        let table = NSTableView::new(mtm);
        let column = NSTableColumn::new(mtm);
        column.setTitle(&NSString::from_str("Dictations"));
        table.addTableColumn(&column);
        table.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle);
        table.setHeaderView(None);
        table.setRowHeight(36.0);
        table.setBackgroundColor(&NSColor::clearColor());
        table.setAllowsMultipleSelection(true);
        unsafe { table.setDataSource(Some(ProtocolObject::from_ref(&*source))) };

        let table_scroll = NSScrollView::new(mtm);
        table_scroll.setHasVerticalScroller(true);
        table_scroll.setAutohidesScrollers(true);
        table_scroll.setDrawsBackground(false);
        table_scroll.setDocumentView(Some(&table));
        let list_card = Surface::new(mtm, NSColor::quaternarySystemFillColor(), 10.0, false);
        list_card.wrap(&table_scroll);

        let search = NSSearchField::new(mtm);
        search.setPlaceholderString(Some(&NSString::from_str("Search dictations")));
        search.setSendsSearchStringImmediately(true);
        let search_action = Action::new(mtm, move |_sender: &AnyObject| {
            delegate.get().reload_history();
        });
        search_action.attach(&search);
        keep(search_action);

        let switch_action = Action::new(mtm, move |sender: &AnyObject| {
            let Some(control) = sender.downcast_ref::<NSSegmentedControl>() else {
                return;
            };
            delegate.get().set_history_mode(control.selectedSegment() == 1);
        });
        let switcher = ui::segmented(mtm, &["Dictations", "Meetings"], 0, &switch_action);
        keep(switch_action);

        let detail = NSTextView::initWithFrame(
            NSTextView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(400.0, 100.0)),
        );
        detail.setEditable(false);
        detail.setSelectable(true);
        detail.setRichText(false);
        detail.setDrawsBackground(false);
        detail.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        detail.setTextContainerInset(NSSize::new(8.0, 10.0));
        detail.setVerticallyResizable(true);
        detail.setHorizontallyResizable(false);
        detail.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        detail.setMinSize(NSSize::new(0.0, 0.0));
        detail.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
        if let Some(container) = unsafe { detail.textContainer() } {
            container.setWidthTracksTextView(true);
        }
        let detail_scroll = NSScrollView::new(mtm);
        detail_scroll.setHasVerticalScroller(true);
        detail_scroll.setAutohidesScrollers(true);
        detail_scroll.setDrawsBackground(false);
        detail_scroll.setDocumentView(Some(&detail));
        let detail_card = Surface::new(mtm, NSColor::quaternarySystemFillColor(), 10.0, false);
        detail_card.wrap(&detail_scroll);
        let detail_card: Retained<NSView> = Retained::into_super(detail_card);

        let chat_toggle_action = Action::new(mtm, move |sender: &AnyObject| {
            let Some(control) = sender.downcast_ref::<NSSegmentedControl>() else {
                return;
            };
            delegate.get().with_history(|history| {
                history.chat_preference.set(Some(control.selectedSegment() == 0));
                history.update_detail();
            });
        });
        let chat_toggle = ui::segmented(mtm, &["Chat", "Transcript"], 0, &chat_toggle_action);
        keep(chat_toggle_action);

        let chat_stack = chat::conversation(mtm);
        let chat_scroll = NSScrollView::new(mtm);
        chat_scroll.setHasVerticalScroller(true);
        chat_scroll.setAutohidesScrollers(true);
        chat_scroll.setDrawsBackground(false);
        chat_scroll.setDocumentView(Some(&chat_stack));
        let content = chat_scroll.contentView();
        let constraints = [
            chat_stack.topAnchor().constraintEqualToAnchor(&content.topAnchor()),
            chat_stack.leadingAnchor().constraintEqualToAnchor(&content.leadingAnchor()),
            chat_stack.trailingAnchor().constraintEqualToAnchor(&content.trailingAnchor()),
            chat_stack.widthAnchor().constraintEqualToAnchor(&content.widthAnchor()),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        let chat_card = Surface::new(mtm, NSColor::quaternarySystemFillColor(), 10.0, false);
        chat_card.wrap(&chat_scroll);
        let chat_card: Retained<NSView> = Retained::into_super(chat_card);
        chat_card.setHidden(true);

        let meta = ui::detail(mtm, "");
        meta.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);

        let button = |title: &str, handler: Box<dyn Fn()>| {
            let action = Action::new(mtm, move |_sender: &AnyObject| handler());
            let button = ui::button(mtm, title, &action);
            actions.borrow_mut().push(action);
            button
        };
        let copy = button("Copy", Box::new(move || delegate.get().copy_selected()));
        let play = button("Play", Box::new(move || delegate.get().play_selected()));
        let delete = button("Delete", Box::new(move || delegate.get().delete_selected()));
        let export = button("Export…", Box::new(move || delegate.get().export_history()));
        let more = button("Load More", Box::new(move || delegate.get().load_more_history()));
        let retry = button("Retry", Box::new(move || delegate.get().rerun_diarization()));

        let rerun_action = Action::new(mtm, move |sender: &AnyObject| {
            let Some(popup) = sender.downcast_ref::<NSPopUpButton>() else {
                return;
            };
            let Some(engine) = popup.titleOfSelectedItem().map(|title| title.to_string()) else {
                return;
            };
            delegate.get().rerun_selected(&engine);
        });
        let rerun = ui::popup(mtm, &["Re-run"], 0, &rerun_action);
        keep(rerun_action);

        let regen_action =
            Action::new(mtm, move |_: &AnyObject| delegate.get().regenerate_selected_summary());
        let regen = ui::button(mtm, "Regenerate summary", &regen_action);
        keep(regen_action);

        let toolbar = ui::hstack(mtm, 8.0);
        for control in ui::views![copy, play, retry, rerun, regen, delete, export] {
            toolbar.addArrangedSubview(control);
        }

        let view = NSView::new(mtm);
        for part in [
            &search as &NSView,
            &switcher,
            &list_card,
            &more,
            &meta,
            &detail_card,
            &chat_card,
            &chat_toggle,
            &toolbar,
        ] {
            part.setTranslatesAutoresizingMaskIntoConstraints(false);
            view.addSubview(part);
        }
        let margin = 20.0;
        let constraints = [
            search.topAnchor().constraintEqualToAnchor_constant(&view.topAnchor(), 16.0),
            search.leadingAnchor().constraintEqualToAnchor_constant(&view.leadingAnchor(), margin),
            switcher.centerYAnchor().constraintEqualToAnchor(&search.centerYAnchor()),
            switcher.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), -margin),
            search.trailingAnchor().constraintEqualToAnchor_constant(&switcher.leadingAnchor(), -12.0),
            list_card.topAnchor().constraintEqualToAnchor_constant(&search.bottomAnchor(), 12.0),
            list_card.leadingAnchor().constraintEqualToAnchor_constant(&view.leadingAnchor(), margin),
            list_card.widthAnchor().constraintEqualToConstant(LIST_WIDTH),
            list_card.bottomAnchor().constraintEqualToAnchor_constant(&more.topAnchor(), -10.0),
            more.leadingAnchor().constraintEqualToAnchor_constant(&view.leadingAnchor(), margin),
            more.bottomAnchor().constraintEqualToAnchor_constant(&view.bottomAnchor(), -16.0),
            meta.topAnchor().constraintEqualToAnchor(&list_card.topAnchor()),
            meta.leadingAnchor().constraintEqualToAnchor_constant(&list_card.trailingAnchor(), 16.0),
            meta.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), -margin),
            chat_toggle.trailingAnchor().constraintEqualToAnchor(&meta.trailingAnchor()),
            chat_toggle.topAnchor().constraintEqualToAnchor_constant(&meta.bottomAnchor(), 6.0),
            detail_card.topAnchor().constraintEqualToAnchor_constant(&chat_toggle.bottomAnchor(), 8.0),
            detail_card.leadingAnchor().constraintEqualToAnchor(&meta.leadingAnchor()),
            detail_card.trailingAnchor().constraintEqualToAnchor(&meta.trailingAnchor()),
            detail_card.bottomAnchor().constraintEqualToAnchor_constant(&toolbar.topAnchor(), -10.0),
            chat_card.topAnchor().constraintEqualToAnchor(&detail_card.topAnchor()),
            chat_card.leadingAnchor().constraintEqualToAnchor(&detail_card.leadingAnchor()),
            chat_card.trailingAnchor().constraintEqualToAnchor(&detail_card.trailingAnchor()),
            chat_card.bottomAnchor().constraintEqualToAnchor(&detail_card.bottomAnchor()),
            toolbar.leadingAnchor().constraintEqualToAnchor(&meta.leadingAnchor()),
            toolbar.bottomAnchor().constraintEqualToAnchor_constant(&view.bottomAnchor(), -16.0),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));

        let history = Self {
            view,
            mtm,
            source,
            table,
            search,
            switcher,
            detail,
            detail_card,
            chat_card,
            chat_stack,
            chat_toggle,
            meta,
            play,
            copy,
            delete,
            rerun,
            regen,
            retry,
            more,
            actions,
            delegate,
            mode: Cell::new(ListMode::Dictations),
            summaries: RefCell::new(Vec::new()),
            meetings: RefCell::new(Vec::new()),
            selected: Cell::new(None),
            selected_meeting: Cell::new(None),
            before: Cell::new(None),
            exhausted: Cell::new(false),
            meeting_before: Cell::new(None),
            meeting_exhausted: Cell::new(false),
            chat_preference: Cell::new(None),
            diarization_status: Cell::new(None),
        };
        history.more.setEnabled(false);
        history.populate_engines();
        history
    }

    pub fn view(&self) -> &Retained<NSView> {
        &self.view
    }

    pub fn reload(&self) {
        if self.mode.get() == ListMode::Meetings {
            self.reload_meetings();
            return;
        }
        let query = self.search.stringValue().to_string();
        let query = if query.trim().is_empty() { None } else { Some(query) };
        let rows = match Store::open_read_only(&history_path()) {
            Ok(store) => {
                store.list(&Query { text: query, before: None, limit: Query::PAGE }).unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        let selected = self.selected.get();
        self.source.set_rows(display_rows(&rows));
        *self.summaries.borrow_mut() = rows.clone();
        self.before.set(rows.last().map(|row| row.created_at));
        self.exhausted.set(rows.len() < Query::PAGE as usize);
        self.more.setEnabled(!self.exhausted.get());
        self.table.reloadData();
        if rows.is_empty() {
            self.selected.set(None);
            unsafe { self.table.deselectAll(None) };
            self.clear_detail();
            return;
        }
        let index = selected.and_then(|id| rows.iter().position(|row| row.id == id)).unwrap_or(0);
        self.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
        self.selected.set(Some(rows[index].id));
        self.update_detail();
    }

    fn reload_meetings(&self) {
        let query = self.search.stringValue().to_string();
        let query = if query.trim().is_empty() { None } else { Some(query) };
        let rows = match Store::open_read_only(&history_path()) {
            Ok(store) => store
                .list_meetings(&Query { text: query, before: None, limit: Query::PAGE })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let selected = self.selected_meeting.get();
        self.source.set_rows(display_meeting_rows(&rows));
        *self.meetings.borrow_mut() = rows.clone();
        self.meeting_before.set(rows.last().map(|row| row.started_at));
        self.meeting_exhausted.set(rows.len() < Query::PAGE as usize);
        self.more.setEnabled(!self.meeting_exhausted.get());
        self.table.reloadData();
        if rows.is_empty() {
            self.selected_meeting.set(None);
            unsafe { self.table.deselectAll(None) };
            self.clear_detail();
            return;
        }
        let index = selected.and_then(|id| rows.iter().position(|row| row.id == id)).unwrap_or(0);
        self.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
        self.selected_meeting.set(Some(rows[index].id));
        self.update_detail();
    }

    pub fn set_mode(&self, meetings: bool) {
        let mode = if meetings { ListMode::Meetings } else { ListMode::Dictations };
        if self.mode.get() == mode {
            return;
        }
        self.mode.set(mode);
        self.switcher.setSelectedSegment(if meetings { 1 } else { 0 });
        self.search.setStringValue(&NSString::from_str(""));
        self.search.setPlaceholderString(Some(&NSString::from_str(if meetings {
            "Search meetings"
        } else {
            "Search dictations"
        })));
        self.reload();
    }

    pub fn is_meetings(&self) -> bool {
        self.mode.get() == ListMode::Meetings
    }

    pub fn load_more(&self) {
        if self.mode.get() == ListMode::Meetings {
            self.load_more_meetings();
            return;
        }
        if self.exhausted.get() {
            return;
        }
        let text = self.search.stringValue().to_string();
        let text = if text.trim().is_empty() { None } else { Some(text) };
        let rows = match Store::open_read_only(&history_path()) {
            Ok(store) => {
                store.list(&Query { text, before: self.before.get(), limit: Query::PAGE }).unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        if rows.is_empty() {
            self.exhausted.set(true);
            self.more.setEnabled(false);
            return;
        }
        self.before.set(rows.last().map(|row| row.created_at));
        if rows.len() < Query::PAGE as usize {
            self.exhausted.set(true);
        }
        self.more.setEnabled(!self.exhausted.get());
        self.source.append_rows(display_rows(&rows));
        self.summaries.borrow_mut().extend(rows);
        self.table.reloadData();
        if let Some(id) = self.selected.get() {
            self.select(id);
        }
    }

    fn load_more_meetings(&self) {
        if self.meeting_exhausted.get() {
            return;
        }
        let text = self.search.stringValue().to_string();
        let text = if text.trim().is_empty() { None } else { Some(text) };
        let rows = match Store::open_read_only(&history_path()) {
            Ok(store) => store
                .list_meetings(&Query { text, before: self.meeting_before.get(), limit: Query::PAGE })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if rows.is_empty() {
            self.meeting_exhausted.set(true);
            self.more.setEnabled(false);
            return;
        }
        self.meeting_before.set(rows.last().map(|row| row.started_at));
        if rows.len() < Query::PAGE as usize {
            self.meeting_exhausted.set(true);
        }
        self.more.setEnabled(!self.meeting_exhausted.get());
        self.source.append_rows(display_meeting_rows(&rows));
        self.meetings.borrow_mut().extend(rows);
        self.table.reloadData();
        if let Some(id) = self.selected_meeting.get() {
            self.select_meeting(id);
        }
    }

    pub fn selected_id(&self) -> Option<TakeId> {
        if self.is_meetings() {
            return None;
        }
        let row = self.table.selectedRow();
        if row < 0 {
            return None;
        }
        self.summaries.borrow().get(row as usize).map(|summary| summary.id)
    }

    pub fn selected_ids(&self) -> Vec<TakeId> {
        if self.is_meetings() {
            return Vec::new();
        }
        let indexes = self.table.selectedRowIndexes();
        let summaries = self.summaries.borrow();
        (0..summaries.len())
            .filter(|index| indexes.containsIndex(*index))
            .map(|index| summaries[index].id)
            .collect()
    }

    pub fn visible_rows(&self) -> Vec<TakeSummary> {
        self.summaries.borrow().clone()
    }

    pub fn selected_meeting_id(&self) -> Option<MeetingId> {
        if !self.is_meetings() {
            return None;
        }
        let row = self.table.selectedRow();
        if row < 0 {
            return None;
        }
        self.meetings.borrow().get(row as usize).map(|summary| summary.id)
    }

    pub fn selected_meeting_ids(&self) -> Vec<MeetingId> {
        if !self.is_meetings() {
            return Vec::new();
        }
        let indexes = self.table.selectedRowIndexes();
        let meetings = self.meetings.borrow();
        (0..meetings.len())
            .filter(|index| indexes.containsIndex(*index))
            .map(|index| meetings[index].id)
            .collect()
    }

    pub fn visible_meetings(&self) -> Vec<MeetingSummary> {
        self.meetings.borrow().clone()
    }

    pub fn selected_meeting(&self) -> Option<tf_store::Meeting> {
        let id = self.selected_meeting.get()?;
        Store::open_read_only(&history_path()).ok()?.meeting(id).ok().flatten()
    }

    pub fn select_meeting(&self, id: MeetingId) {
        if !self.is_meetings() {
            self.set_mode(true);
        }
        let index = self.meetings.borrow().iter().position(|row| row.id == id);
        if let Some(index) = index {
            self.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
            self.selected_meeting.set(Some(id));
            self.update_detail();
        } else {
            self.reload_meetings();
            if let Some(index) = self.meetings.borrow().iter().position(|row| row.id == id) {
                self.table
                    .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
                self.selected_meeting.set(Some(id));
                self.update_detail();
            }
        }
    }

    pub fn meeting_segment_added(&self, id: MeetingId) {
        if self.is_meetings() && self.selected_meeting.get() == Some(id) {
            self.update_detail();
        }
    }

    pub fn select(&self, id: TakeId) {
        if self.is_meetings() {
            self.set_mode(false);
        }
        let index = self.summaries.borrow().iter().position(|row| row.id == id);
        if let Some(index) = index {
            self.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
            self.selected.set(Some(id));
            self.update_detail();
        }
    }

    fn selected_take(&self) -> Option<tf_store::Take> {
        let id = self.selected.get()?;
        Store::open_read_only(&history_path()).ok()?.get(id).ok().flatten()
    }

    pub fn tick(&self) {
        let row = self.table.selectedRow();
        if row < 0 {
            return;
        }
        if self.is_meetings() {
            let len = self.meetings.borrow().len();
            if (row as usize) + 20 >= len && !self.meeting_exhausted.get() {
                self.load_more_meetings();
            }
            let id = self.meetings.borrow().get(row as usize).map(|summary| summary.id);
            if id != self.selected_meeting.get() {
                self.selected_meeting.set(id);
                self.update_detail();
            }
            return;
        }
        let len = self.summaries.borrow().len();
        if (row as usize) + 20 >= len && !self.exhausted.get() {
            self.load_more();
        }
        let id = self.summaries.borrow().get(row as usize).map(|summary| summary.id);
        if id != self.selected.get() {
            self.selected.set(id);
            self.update_detail();
        }
    }

    fn update_detail(&self) {
        if self.is_meetings() {
            self.update_meeting_detail();
            return;
        }
        let Some(take) = self.selected_take() else {
            self.clear_detail();
            return;
        };
        self.chat_card.setHidden(true);
        self.detail_card.setHidden(false);
        self.chat_toggle.setEnabled(false);
        self.selected.set(Some(take.id));
        let mut text = take.final_text.clone();
        if take.raw_text != take.final_text {
            text.push_str("\n\nOriginal transcript\n");
            text.push_str(&take.raw_text);
        }
        if let Some(parent) = take.parent_id {
            text.push_str(&format!("\n\nRe-run of take {parent}"));
        }
        self.detail.setString(&NSString::from_str(&text));
        let app = take.app_name.clone().or(take.app_bundle_id.clone()).unwrap_or_else(|| "tf".into());
        let mode = take.mode_name.clone().unwrap_or_else(|| "Raw".into());
        let stt = take.stt_ms.map(|ms| format!(" · STT {ms} ms")).unwrap_or_default();
        let llm = take.llm_ms.map(|ms| format!(" · LLM {ms} ms")).unwrap_or_default();
        let parent = take.parent_id.map(|id| format!(" · re-run of {id}")).unwrap_or_default();
        self.meta.setStringValue(&NSString::from_str(&format!(
            "{app} · {mode} · {} · {}{stt}{llm}{parent}",
            take.engine,
            format_duration(take.duration_ms)
        )));

        let has_audio = take.audio_file.is_some();
        self.play.setEnabled(has_audio);
        self.rerun.setEnabled(has_audio);
        self.regen.setEnabled(false);
        self.copy.setEnabled(true);
        self.delete.setEnabled(true);
    }

    fn update_meeting_detail(&self) {
        let Some(meeting) = self.selected_meeting() else {
            self.clear_detail();
            return;
        };
        self.selected_meeting.set(Some(meeting.id));
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
            .map(|(segment, label)| summary::Line {
                speaker: label.as_str(),
                start_ms: segment.start_ms,
                text: &segment.text,
            })
            .collect::<Vec<_>>();
        let transcript = summary::transcript(lines);
        let mut text = String::new();
        if let Some(summary) = meeting.summary.as_deref() {
            text.push_str(summary.trim());
            text.push_str("\n\n## Transcript\n");
        } else {
            text.push_str("The summary is not ready yet.\n\n");
        }
        text.push_str(&transcript);
        self.detail.setString(&NSString::from_str(&text));
        let app = meeting.app_name.clone().or(meeting.app_bundle_id.clone()).unwrap_or_else(|| "tf".into());
        let status = match meeting.status {
            tf_store::MeetingStatus::Recording => "recording",
            tf_store::MeetingStatus::Processing => "summary pending",
            tf_store::MeetingStatus::Done => "done",
            tf_store::MeetingStatus::Interrupted => "interrupted",
        };
        let ended = meeting.ended_at.unwrap_or_else(|| {
            segments
                .last()
                .map(|segment| meeting.started_at + segment.end_ms as i64)
                .unwrap_or(meeting.started_at)
        });
        let duration = (ended - meeting.started_at).max(0) as u64;
        let progress = self.diarization_status.get().filter(|(id, _)| *id == meeting.id);
        let diarization = match (meeting.diarization, progress) {
            (Some(tf_store::DiarizationState::Running), Some((_, fraction))) => {
                format!(" · identifying speakers… {:.0}%", fraction * 100.0)
            }
            (Some(tf_store::DiarizationState::Running), None) => " · identifying speakers…".to_owned(),
            (Some(tf_store::DiarizationState::Failed), _) => " · speakers failed".to_owned(),
            _ => String::new(),
        };
        self.meta.setStringValue(&NSString::from_str(&format!(
            "{} · {app} · {status} · {}{diarization}",
            meeting.title,
            format_duration(duration)
        )));

        let chat_available = !segments.is_empty();
        let show_chat = self.chat_preference.get().unwrap_or(true) && chat_available;
        self.chat_toggle.setEnabled(chat_available);
        self.chat_toggle.setSelectedSegment(if show_chat { 0 } else { 1 });
        self.detail_card.setHidden(show_chat);
        self.chat_card.setHidden(!show_chat);
        if show_chat {
            self.build_chat(
                meeting.id,
                &chat_date(meeting.started_at),
                &segments,
                &speakers,
                meeting.audio_them.is_some(),
            );
        }

        self.play.setEnabled(meeting.audio_me.is_some() || meeting.audio_them.is_some());
        self.copy.setEnabled(true);
        self.delete.setEnabled(true);
        self.rerun.setEnabled(false);
        self.regen.setEnabled(!segments.is_empty());
        let has_voice_data = Store::open_read_only(&history_path())
            .ok()
            .and_then(|store| store.segment_embeddings(meeting.id).ok())
            .is_some_and(|rows| rows.iter().any(|row| row.3.is_some()));
        let rerunnable = (meeting.audio_them.is_some() || has_voice_data)
            && matches!(
                meeting.diarization,
                Some(tf_store::DiarizationState::Failed | tf_store::DiarizationState::Done)
            );
        self.retry.setEnabled(rerunnable);
        self.retry.setTitle(&NSString::from_str(
            if meeting.diarization == Some(tf_store::DiarizationState::Failed) {
                "Retry"
            } else {
                "Re-run speakers"
            },
        ));
    }

    pub fn set_diarization_status(&self, meeting_id: MeetingId, fraction: Option<f32>) {
        self.diarization_status.set(fraction.map(|fraction| (meeting_id, fraction)));
        if self.selected_meeting.get() == Some(meeting_id) {
            self.update_meeting_detail();
        }
    }

    fn build_chat(
        &self,
        meeting_id: MeetingId,
        started_at: &str,
        segments: &[Segment],
        speakers: &[MeetingSpeaker],
        has_audio: bool,
    ) {
        let mtm = self.mtm;
        for view in self.chat_stack.arrangedSubviews().iter() {
            self.chat_stack.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }

        chat::push(&self.chat_stack, &chat::separator(mtm, &started_at));
        if !speakers.is_empty() {
            chat::push(
                &self.chat_stack,
                &speaker_bar(mtm, self.delegate, meeting_id, speakers, segments, &self.actions),
            );
        }

        let labels = speakers
            .iter()
            .map(|speaker| (speaker.id, speaker.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let mut bubbles: Vec<Bubble> = Vec::new();
        for segment in segments {
            let is_me = segment.speaker == tf_store::Speaker::Me;
            let speaker = labels.get(&segment.speaker_id.unwrap_or(-1)).cloned();
            let label = if is_me {
                "Me".to_owned()
            } else {
                speaker.as_ref().map(MeetingSpeaker::label).unwrap_or_else(|| "Them".to_owned())
            };
            let color = if is_me { 8 } else { speaker.as_ref().map(|speaker| speaker.color).unwrap_or(0) };
            match bubbles.last_mut() {
                Some(last)
                    if last.is_me == is_me
                        && last.speaker_id == segment.speaker_id
                        && segment.start_ms.saturating_sub(last.end_ms) <= 30_000 =>
                {
                    last.end_ms = segment.end_ms;
                    last.texts.push(segment.text.trim().to_owned());
                    last.overlap |= segment.overlap;
                    last.segment_ids.push(segment.id);
                }
                _ => bubbles.push(Bubble {
                    is_me,
                    speaker_id: segment.speaker_id,
                    label,
                    color,
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                    texts: vec![segment.text.trim().to_owned()],
                    overlap: segment.overlap,
                    segment_ids: vec![segment.id],
                }),
            }
        }

        for bubble in &bubbles {
            let mut previous: Option<Retained<NSView>> = None;
            for (index, text) in bubble.texts.iter().enumerate() {
                let row = bubble_row(
                    mtm,
                    self.delegate,
                    meeting_id,
                    bubble,
                    text,
                    index > 0,
                    speakers,
                    has_audio,
                    &self.actions,
                );
                if let Some(previous) = previous.as_ref() {
                    self.chat_stack.setCustomSpacing_afterView(chat::TIGHT_GAP, previous);
                }
                chat::push(&self.chat_stack, &row);
                previous = Some(row);
            }
        }
        if bubbles.is_empty() {
            self.chat_stack
                .addArrangedSubview(&ui::detail(mtm, "No speech was transcribed for this meeting."));
        }
    }

    fn clear_detail(&self) {
        self.detail.setString(&NSString::from_str(""));
        self.meta.setStringValue(&NSString::from_str(""));
        self.play.setEnabled(false);
        self.copy.setEnabled(false);
        self.delete.setEnabled(false);
        self.rerun.setEnabled(false);
        self.regen.setEnabled(false);
        self.retry.setEnabled(false);
        self.chat_toggle.setEnabled(false);
        self.detail_card.setHidden(false);
        self.chat_card.setHidden(true);
    }

    fn populate_engines(&self) {
        self.rerun.removeAllItems();
        let mut engines = Vec::new();
        for model in tf_models::scan(&tf_core::paths::models_dir()) {
            engines.push(model.id);
        }
        let current = self.delegate.get().config().engine.default;
        if !engines.contains(&current) {
            engines.push(current);
        }
        for engine in engines {
            self.rerun.addItemWithTitle(&NSString::from_str(&engine));
        }
    }
}

fn display_meeting_rows(rows: &[MeetingSummary]) -> Vec<String> {
    let formatter = NSDateFormatter::new();
    formatter.setDateFormat(Some(&NSString::from_str("d MMM HH:mm")));
    rows.iter()
        .map(|row| {
            let preview = row
                .preview
                .as_deref()
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let preview = if preview.chars().count() > 46 {
                format!("{}…", preview.chars().take(46).collect::<String>())
            } else {
                preview
            };
            let status = match row.status {
                tf_store::MeetingStatus::Recording => " · rec",
                tf_store::MeetingStatus::Processing => " · …",
                tf_store::MeetingStatus::Interrupted => " · interrupted",
                tf_store::MeetingStatus::Done => "",
            };
            format!("{}  {}{status} — {preview}", format_time(&formatter, row.started_at), row.title)
        })
        .collect()
}

fn display_rows(rows: &[TakeSummary]) -> Vec<String> {
    let formatter = NSDateFormatter::new();
    formatter.setDateFormat(Some(&NSString::from_str("d MMM HH:mm")));
    rows.iter().map(|row| format_summary(row, &formatter)).collect()
}

fn format_summary(row: &TakeSummary, formatter: &NSDateFormatter) -> String {
    let app = row.app_name.clone().or(row.app_bundle_id.clone()).unwrap_or_else(|| "tf".into());
    let text = row.final_text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if text.chars().count() > 54 {
        format!("{}…", text.chars().take(54).collect::<String>())
    } else {
        text
    };
    format!("{}  {app} — {text}", format_time(formatter, row.created_at))
}

fn chat_date(unix_ms: i64) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setDateFormat(Some(&NSString::from_str("d MMM yyyy 'at' HH:mm")));
    format_time(&formatter, unix_ms)
}

fn format_time(formatter: &NSDateFormatter, unix_ms: i64) -> String {
    let date = NSDate::dateWithTimeIntervalSince1970(unix_ms as f64 / 1000.0);
    formatter.stringFromDate(&date).to_string()
}

fn format_duration(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}:{:02}", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

struct Bubble {
    is_me: bool,
    speaker_id: Option<i64>,
    label: String,
    color: u8,
    start_ms: u64,
    end_ms: u64,
    texts: Vec<String>,
    overlap: bool,
    segment_ids: Vec<i64>,
}

enum ChipAction {
    Rename(i64),
    Play(i64),
    Merge { from: i64, into: i64 },
    Split(i64),
}

enum BubbleAction {
    Assign { segment_ids: Vec<i64>, speaker_id: i64 },
    NewSpeaker(Vec<i64>),
    Copy(String),
}

fn talk_time(segments: &[Segment], speaker_id: Option<i64>) -> u64 {
    segments
        .iter()
        .filter(|segment| segment.speaker_id == speaker_id)
        .map(|segment| segment.end_ms.saturating_sub(segment.start_ms))
        .sum()
}

fn speaker_bar(
    mtm: MainThreadMarker,
    delegate: Delegate,
    meeting_id: MeetingId,
    speakers: &[MeetingSpeaker],
    segments: &[Segment],
    actions: &RefCell<Vec<Action>>,
) -> Retained<NSView> {
    let bar = ui::hstack(mtm, 10.0);
    for speaker in speakers {
        let time = format_duration(talk_time(segments, Some(speaker.id)));
        let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, true);
        popup.addItemWithTitle(&NSString::from_str(&format!("● {} · {time}", speaker.label())));
        popup.addItemWithTitle(&NSString::from_str("Rename…"));
        popup.addItemWithTitle(&NSString::from_str("Play a sample"));
        let mut codes = vec![ChipAction::Rename(speaker.id), ChipAction::Play(speaker.id)];
        for other in speakers.iter().filter(|other| other.id != speaker.id) {
            popup.addItemWithTitle(&NSString::from_str(&format!("Merge into {}", other.label())));
            codes.push(ChipAction::Merge { from: speaker.id, into: other.id });
        }
        popup.addItemWithTitle(&NSString::from_str("Split…"));
        codes.push(ChipAction::Split(speaker.id));
        let action = Action::new(mtm, move |sender: &AnyObject| {
            let Some(popup) = sender.downcast_ref::<NSPopUpButton>() else {
                return;
            };
            let index = popup.indexOfSelectedItem();
            if index <= 0 {
                return;
            }
            match codes.get(index as usize - 1) {
                Some(ChipAction::Rename(id)) => delegate.get().rename_speaker(*id),
                Some(ChipAction::Play(id)) => delegate.get().play_speaker_sample(meeting_id, *id),
                Some(ChipAction::Merge { from, into }) => {
                    delegate.get().speaker_edit(tf_core::SpeakerEdit::Merge { from: *from, into: *into })
                }
                Some(ChipAction::Split(id)) => delegate.get().split_speaker(*id),
                None => {}
            }
        });
        action.attach(&popup);
        actions.borrow_mut().push(action);
        bar.addArrangedSubview(&popup);
    }
    let me = format_duration(talk_time(segments, None));
    let label = ui::detail(mtm, &format!("● Me · {me}"));
    label.setTextColor(Some(&NSColor::controlAccentColor()));
    bar.addArrangedSubview(&label);
    Retained::into_super(bar)
}

#[allow(clippy::too_many_arguments)]
fn bubble_row(
    mtm: MainThreadMarker,
    delegate: Delegate,
    meeting_id: MeetingId,
    bubble: &Bubble,
    text: &str,
    continued: bool,
    speakers: &[MeetingSpeaker],
    has_audio: bool,
    actions: &RefCell<Vec<Action>>,
) -> Retained<NSView> {
    let mut controls: Vec<Retained<NSView>> = Vec::new();
    if bubble.overlap && !continued {
        let marker = ui::detail(mtm, "⧉");
        marker.setToolTip(Some(&NSString::from_str("Several people were speaking")));
        controls.push(Retained::into_super(Retained::into_super(marker)));
    }
    if !bubble.is_me && has_audio && !continued {
        let play_action = Action::new(mtm, {
            let (start, end) = (bubble.start_ms, bubble.end_ms);
            move |_sender: &AnyObject| delegate.get().play_meeting_range(meeting_id, start, end)
        });
        let play = ui::button(mtm, "▸", &play_action);
        play.setBordered(false);
        actions.borrow_mut().push(play_action);
        controls.push(Retained::into_super(Retained::into_super(play)));
    }
    if !bubble.is_me && !continued {
        let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, true);
        popup.addItemWithTitle(&NSString::from_str("⋯"));
        let mut codes = Vec::new();
        for speaker in speakers.iter().filter(|speaker| Some(speaker.id) != bubble.speaker_id) {
            popup.addItemWithTitle(&NSString::from_str(&format!("Assign to {}", speaker.label())));
            codes.push(BubbleAction::Assign {
                segment_ids: bubble.segment_ids.clone(),
                speaker_id: speaker.id,
            });
        }
        popup.addItemWithTitle(&NSString::from_str("New speaker"));
        codes.push(BubbleAction::NewSpeaker(bubble.segment_ids.clone()));
        popup.addItemWithTitle(&NSString::from_str("Copy"));
        codes.push(BubbleAction::Copy(bubble.texts.join("\n")));
        let action = Action::new(mtm, move |sender: &AnyObject| {
            let Some(popup) = sender.downcast_ref::<NSPopUpButton>() else {
                return;
            };
            let index = popup.indexOfSelectedItem();
            if index <= 0 {
                return;
            }
            match codes.get(index as usize - 1) {
                Some(BubbleAction::Assign { segment_ids, speaker_id }) => {
                    delegate.get().speaker_edit(tf_core::SpeakerEdit::Assign {
                        segment_ids: segment_ids.clone(),
                        speaker_id: *speaker_id,
                    })
                }
                Some(BubbleAction::NewSpeaker(segment_ids)) => delegate
                    .get()
                    .speaker_edit(tf_core::SpeakerEdit::NewSpeaker { segment_ids: segment_ids.clone() }),
                Some(BubbleAction::Copy(text)) => delegate.get().copy_text(text),
                None => {}
            }
        });
        action.attach(&popup);
        actions.borrow_mut().push(action);
        controls.push(Retained::into_super(Retained::into_super(Retained::into_super(popup))));
    }
    let time = summary::clock(bubble.start_ms);
    chat::row(
        mtm,
        chat::Bubble {
            is_me: bubble.is_me,
            name: &bubble.label,
            name_color: speaker_text_color(bubble.color),
            time: &time,
            text,
            max_width: 420.0,
            continued,
            controls,
        },
    )
}

fn speaker_text_color(index: u8) -> Retained<NSColor> {
    base_color(index)
}

fn base_color(index: u8) -> Retained<NSColor> {
    match index % 9 {
        0 => NSColor::systemBlueColor(),
        1 => NSColor::systemOrangeColor(),
        2 => NSColor::systemPurpleColor(),
        3 => NSColor::systemGreenColor(),
        4 => NSColor::systemPinkColor(),
        5 => NSColor::systemTealColor(),
        6 => NSColor::systemYellowColor(),
        7 => NSColor::systemIndigoColor(),
        _ => NSColor::controlAccentColor(),
    }
}
