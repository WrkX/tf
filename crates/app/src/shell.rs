use std::cell::Cell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFocusRingType, NSFont, NSLayoutConstraint, NSSearchField,
    NSSplitViewController, NSSplitViewItem, NSTextField, NSView, NSViewController, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

use crate::ui::{self, Action, Surface};

const WINDOW_SIZE: NSSize = NSSize::new(820.0, 600.0);
const MIN_SIZE: NSSize = NSSize::new(680.0, 420.0);
const SIDEBAR_WIDTH: f64 = 220.0;
const TILE: f64 = 22.0;
const ROW_PADDING: f64 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    General,
    Permissions,
    Audio,
    Models,
    Modes,
    Vocabulary,
    Meetings,
    History,
    Storage,
    About,
}

impl Page {
    pub const ALL: [Page; 10] = [
        Page::General,
        Page::Permissions,
        Page::Audio,
        Page::Models,
        Page::Modes,
        Page::Vocabulary,
        Page::Meetings,
        Page::History,
        Page::Storage,
        Page::About,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }

    /// liest den entwickler-schalter `TF_OPEN_SETTINGS`.
    pub fn from_name(name: &str) -> Page {
        Self::ALL.into_iter().find(|page| page.title().eq_ignore_ascii_case(name)).unwrap_or(Page::General)
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Permissions => "Permissions",
            Page::Audio => "Audio",
            Page::Models => "Models",
            Page::Modes => "Modes",
            Page::Vocabulary => "Vocabulary",
            Page::Meetings => "Meetings",
            Page::History => "History",
            Page::Storage => "Storage",
            Page::About => "About",
        }
    }

    fn tint(self) -> Retained<NSColor> {
        match self {
            Page::General => NSColor::systemGrayColor(),
            Page::Permissions => NSColor::systemBlueColor(),
            Page::Audio => NSColor::systemRedColor(),
            Page::Models => NSColor::systemPurpleColor(),
            Page::Modes => NSColor::systemOrangeColor(),
            Page::Vocabulary => NSColor::systemGreenColor(),
            Page::Meetings => NSColor::systemPinkColor(),
            Page::History => NSColor::systemIndigoColor(),
            Page::Storage => NSColor::systemTealColor(),
            Page::About => NSColor::systemGrayColor(),
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Page::General => "switch.2",
            Page::Permissions => "lock.shield.fill",
            Page::Audio => "mic.fill",
            Page::Models => "cpu",
            Page::Modes => "wand.and.stars",
            Page::Vocabulary => "character.book.closed.fill",
            Page::Meetings => "person.2.wave.2.fill",
            Page::History => "clock.fill",
            Page::Storage => "externaldrive.fill",
            Page::About => "info.circle.fill",
        }
    }
}

const GROUPS: [(&str, &[Page]); 4] = [
    ("General", &[Page::General, Page::Permissions]),
    ("Dictation", &[Page::Audio, Page::Models, Page::Modes, Page::Vocabulary]),
    ("Data", &[Page::Meetings, Page::History, Page::Storage]),
    ("", &[Page::About]),
];

struct State {
    window: Retained<NSWindow>,
    pages: Vec<Retained<NSView>>,
    highlights: Vec<Retained<Surface>>,
    rows: Vec<Retained<NSView>>,
    headers: Vec<(Retained<NSTextField>, &'static [Page])>,
    selected: Cell<Page>,
}

impl State {
    fn select(&self, page: Page) {
        self.selected.set(page);
        for (index, view) in self.pages.iter().enumerate() {
            view.setHidden(index != page.index());
        }
        for (index, highlight) in self.highlights.iter().enumerate() {
            let color =
                if index == page.index() { NSColor::quaternaryLabelColor() } else { NSColor::clearColor() };
            highlight.set_fill(color);
        }
        self.window.setTitle(&NSString::from_str(page.title()));
    }

    fn filter(&self, query: &str) {
        let query = query.trim().to_lowercase();
        let matches = |page: Page| query.is_empty() || page.title().to_lowercase().contains(&query);
        for page in Page::ALL {
            self.rows[page.index()].setHidden(!matches(page));
        }
        for (header, pages) in &self.headers {
            header.setHidden(!pages.iter().any(|page| matches(*page)));
        }
    }
}

pub struct Shell {
    state: Rc<State>,
    _actions: Vec<Action>,
}

impl Shell {
    /// `pages` muss in der reihenfolge von `Page::ALL` übergeben werden.
    pub fn new(mtm: MainThreadMarker, pages: Vec<Retained<NSView>>) -> Self {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, WINDOW_SIZE),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setMinSize(MIN_SIZE);

        let content = NSView::new(mtm);
        for page in &pages {
            content.addSubview(page);
            let guide = content.safeAreaLayoutGuide();
            page.setTranslatesAutoresizingMaskIntoConstraints(false);
            let constraints = [
                page.topAnchor().constraintEqualToAnchor(&guide.topAnchor()),
                page.leadingAnchor().constraintEqualToAnchor(&content.leadingAnchor()),
                page.trailingAnchor().constraintEqualToAnchor(&content.trailingAnchor()),
                page.bottomAnchor().constraintEqualToAnchor(&content.bottomAnchor()),
            ];
            NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
        }

        let mut highlights: Vec<Option<Retained<Surface>>> = Page::ALL.iter().map(|_| None).collect();
        let mut rows: Vec<Option<Retained<NSView>>> = Page::ALL.iter().map(|_| None).collect();
        let mut headers = Vec::new();
        let mut actions = Vec::new();
        let state: Rc<std::cell::OnceCell<Rc<State>>> = Rc::new(std::cell::OnceCell::new());

        let sidebar = ui::vstack(mtm, 3.0);
        sidebar.setEdgeInsets(ui::insets(48.0, 10.0, 12.0, 10.0));

        let search = NSSearchField::new(mtm);
        search.setPlaceholderString(Some(&NSString::from_str("Search")));
        search.setSendsSearchStringImmediately(true);
        let target = Rc::clone(&state);
        let action = Action::new(mtm, move |sender: &AnyObject| {
            if let (Some(state), Some(field)) = (target.get(), sender.downcast_ref::<NSSearchField>()) {
                state.filter(&field.stringValue().to_string());
            }
        });
        action.attach(&search);
        actions.push(action);
        sidebar.addArrangedSubview(&search);
        ui::fill_width(&sidebar, &search, 20.0);
        sidebar.setCustomSpacing_afterView(16.0, &search);
        for (title, group) in GROUPS {
            let header = ui::label(mtm, title);
            header.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
            header.setTextColor(Some(&NSColor::secondaryLabelColor()));
            if !title.is_empty() {
                sidebar.addArrangedSubview(&header);
                sidebar.setCustomSpacing_afterView(4.0, &header);
                headers.push((header.clone(), group));
            }
            for page in group.iter().copied() {
                let target = Rc::clone(&state);
                let action = Action::new(mtm, move |_| {
                    if let Some(state) = target.get() {
                        state.select(page);
                    }
                });
                let (row, highlight) = sidebar_row(mtm, page, &action);
                sidebar.addArrangedSubview(&row);
                ui::fill_width(&sidebar, &row, 20.0);
                highlights[page.index()] = Some(highlight);
                rows[page.index()] = Some(row);
                actions.push(action);
            }
            sidebar.setCustomSpacing_afterView(
                14.0,
                &sidebar.arrangedSubviews().lastObject().expect("group has rows"),
            );
        }

        let sidebar_view = NSView::new(mtm);
        sidebar_view.addSubview(&sidebar);
        let version = ui::label(mtm, &format!("tf {}", env!("CARGO_PKG_VERSION")));
        version.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        version.setTextColor(Some(&NSColor::tertiaryLabelColor()));
        sidebar_view.addSubview(&version);
        let constraints = [
            version.leadingAnchor().constraintEqualToAnchor_constant(&sidebar_view.leadingAnchor(), 18.0),
            version.bottomAnchor().constraintEqualToAnchor_constant(&sidebar_view.bottomAnchor(), -14.0),
            sidebar.topAnchor().constraintEqualToAnchor(&sidebar_view.topAnchor()),
            sidebar.leadingAnchor().constraintEqualToAnchor(&sidebar_view.leadingAnchor()),
            sidebar.trailingAnchor().constraintEqualToAnchor(&sidebar_view.trailingAnchor()),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));

        let sidebar_controller = NSViewController::new(mtm);
        sidebar_controller.setView(&sidebar_view);
        let content_controller = NSViewController::new(mtm);
        content_controller.setView(&content);

        let sidebar_item = NSSplitViewItem::sidebarWithViewController(&sidebar_controller);
        sidebar_item.setMinimumThickness(SIDEBAR_WIDTH);
        sidebar_item.setMaximumThickness(SIDEBAR_WIDTH);
        sidebar_item.setCanCollapse(false);
        let content_item = NSSplitViewItem::contentListWithViewController(&content_controller);

        let split = NSSplitViewController::new(mtm);
        split.addSplitViewItem(&sidebar_item);
        split.addSplitViewItem(&content_item);
        window.setContentViewController(Some(&split));
        window.setContentSize(WINDOW_SIZE);
        window.center();

        let shared = Rc::new(State {
            window,
            pages,
            highlights: highlights.into_iter().flatten().collect(),
            rows: rows.into_iter().flatten().collect(),
            headers,
            selected: Cell::new(Page::General),
        });
        let _ = state.set(Rc::clone(&shared));
        shared.select(Page::General);
        Self { state: shared, _actions: actions }
    }

    pub fn window(&self) -> &NSWindow {
        &self.state.window
    }

    pub fn selected(&self) -> Page {
        self.state.selected.get()
    }

    pub fn select(&self, page: Page) {
        self.state.select(page);
    }
}

fn sidebar_row(mtm: MainThreadMarker, page: Page, action: &Action) -> (Retained<NSView>, Retained<Surface>) {
    let highlight = Surface::new(mtm, NSColor::clearColor(), 8.0, false);

    let tile = ui::icon_tile(mtm, page.symbol(), page.tint(), TILE, 6.0);

    let title = ui::label(mtm, page.title());
    title.setFont(Some(&NSFont::systemFontOfSize(13.0)));

    let row = ui::hstack(mtm, 8.0);
    row.setEdgeInsets(ui::insets(0.0, 8.0, 0.0, 8.0));
    row.addArrangedSubview(&tile);
    row.addArrangedSubview(&title);
    highlight.wrap(&row);

    let click = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(""), None, None, mtm) };
    action.attach(&click);
    click.setTransparent(true);
    click.setFocusRingType(NSFocusRingType::None);
    let container = NSView::new(mtm);
    container.addSubview(&highlight);
    container.addSubview(&click);
    ui::pin_edges(&highlight, &container);
    ui::pin_edges(&click, &container);
    ui::sized(&container, None, Some(TILE + 2.0 * ROW_PADDING));
    (container, highlight)
}
