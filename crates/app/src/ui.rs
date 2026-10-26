//! controls halten ihre targets nur schwach, also muss jede `Action` so lange am leben bleiben wie ihre controls.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSCellImagePosition, NSColor, NSControl, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSFocusRingType, NSFont,
    NSImage, NSImageScaling, NSImageView, NSLayoutAttribute, NSLayoutConstraint,
    NSLayoutConstraintOrientation, NSLayoutPriorityDefaultHigh, NSLayoutPriorityDefaultLow, NSPopUpButton,
    NSPopover, NSPopoverBehavior, NSProgressIndicator, NSProgressIndicatorStyle, NSScrollView, NSSearchField,
    NSSegmentedControl, NSSlider, NSStackView, NSSwitch, NSTextField, NSTextView,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSRectEdge, NSSize, NSString};

use tf_core::trigger::{TriggerKey, is_function_key, media_function_key};

const ROW_INSET_X: f64 = 16.0;
const ROW_INSET_Y: f64 = 10.0;
const ROW_MIN_HEIGHT: f64 = 40.0;
const COMPACT_INSET_Y: f64 = 5.0;
const COMPACT_MIN_HEIGHT: f64 = 34.0;
const PAGE_INSET: f64 = 24.0;
const CARD_RADIUS: f64 = 10.0;
const RECORDER_WIDTH: f64 = 190.0;
const RECORDER_HEIGHT: f64 = 26.0;

macro_rules! views {
    ($($view:expr),* $(,)?) => {
        &[$(&*$view as &objc2_app_kit::NSView),*]
    };
}
pub(crate) use views;

type ActionHandler = Box<dyn Fn(&AnyObject)>;
type CaptureHandler = Box<dyn Fn(TriggerKey)>;

pub struct ActionIvars {
    handler: RefCell<Option<ActionHandler>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfActionTarget"]
    #[ivars = ActionIvars]
    pub struct ActionTarget;

    unsafe impl NSObjectProtocol for ActionTarget {}

    impl ActionTarget {
        #[unsafe(method(perform:))]
        fn perform(&self, sender: &AnyObject) {
            if let Some(handler) = self.ivars().handler.borrow().as_ref() {
                handler(sender);
            }
        }
    }
);

impl ActionTarget {
    fn new(mtm: MainThreadMarker, handler: impl Fn(&AnyObject) + 'static) -> Retained<Self> {
        let target =
            Self::alloc(mtm).set_ivars(ActionIvars { handler: RefCell::new(Some(Box::new(handler))) });
        unsafe { msg_send![super(target), init] }
    }
}

/// hält das objective-c-target am leben; `attach` hängt es an ein `NSControl`.
pub struct Action(Retained<ActionTarget>);

impl Action {
    pub fn new(mtm: MainThreadMarker, handler: impl Fn(&AnyObject) + 'static) -> Self {
        Self(ActionTarget::new(mtm, handler))
    }

    pub fn attach(&self, control: &NSControl) {
        unsafe {
            control.setTarget(Some(&self.0));
            control.setAction(Some(sel!(perform:)));
        }
    }
}

pub struct SurfaceIvars {
    fill: RefCell<Retained<NSColor>>,
    radius: f64,
    bordered: bool,
}

define_class!(
    /// abgerundeter layer-hintergrund; dynamische farben werden pro appearance neu aufgelöst.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfSurface"]
    #[ivars = SurfaceIvars]
    pub struct Surface;

    unsafe impl NSObjectProtocol for Surface {}

    impl Surface {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            let Some(layer) = self.layer() else {
                return;
            };
            let ivars = self.ivars();
            layer.setBackgroundColor(Some(&ivars.fill.borrow().CGColor()));
            layer.setCornerRadius(ivars.radius);
            layer.setMasksToBounds(true);
            if ivars.bordered {
                layer.setBorderWidth(0.5);
                layer.setBorderColor(Some(&NSColor::separatorColor().CGColor()));
            }
        }
    }
);

impl Surface {
    pub fn new(
        mtm: MainThreadMarker,
        fill: Retained<NSColor>,
        radius: f64,
        bordered: bool,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SurfaceIvars { fill: RefCell::new(fill), radius, bordered });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setWantsLayer(true);
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this
    }

    pub fn set_fill(&self, fill: Retained<NSColor>) {
        *self.ivars().fill.borrow_mut() = fill;
        self.setNeedsDisplay(true);
    }

    pub fn wrap(&self, content: &NSView) {
        self.addSubview(content);
        pin_edges(content, self);
    }
}

define_class!(
    /// ursprung oben links, damit gescrollte seiten oben beginnen.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfFlippedView"]
    pub struct FlippedView;

    unsafe impl NSObjectProtocol for FlippedView {}

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setTranslatesAutoresizingMaskIntoConstraints(false);
        this
    }
}

pub struct InfoIvars {
    text: String,
}

define_class!(
    #[unsafe(super(NSButton))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfInfoButton"]
    #[ivars = InfoIvars]
    pub struct InfoButton;

    unsafe impl NSObjectProtocol for InfoButton {}

    impl InfoButton {
        #[unsafe(method(showInfo:))]
        fn show_info(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let text = NSTextField::wrappingLabelWithString(&NSString::from_str(&self.ivars().text), mtm);
            text.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            text.setPreferredMaxLayoutWidth(260.0);
            let content = NSView::new(mtm);
            content.addSubview(&text);
            text.setTranslatesAutoresizingMaskIntoConstraints(false);
            let constraints = [
                text.topAnchor().constraintEqualToAnchor_constant(&content.topAnchor(), 12.0),
                text.leadingAnchor().constraintEqualToAnchor_constant(&content.leadingAnchor(), 14.0),
                text.trailingAnchor().constraintEqualToAnchor_constant(&content.trailingAnchor(), -14.0),
                text.bottomAnchor().constraintEqualToAnchor_constant(&content.bottomAnchor(), -12.0),
                text.widthAnchor().constraintEqualToConstant(260.0),
            ];
            NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
            let controller = NSViewController::new(mtm);
            controller.setView(&content);
            let popover = NSPopover::new(mtm);
            popover.setBehavior(NSPopoverBehavior::Transient);
            popover.setContentViewController(Some(&controller));
            popover.showRelativeToRect_ofView_preferredEdge(self.bounds(), self, NSRectEdge::MaxY);
        }
    }
);

impl InfoButton {
    pub fn new(mtm: MainThreadMarker, text: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(InfoIvars { text: text.to_owned() });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("info.circle"),
            Some(&NSString::from_str("More information")),
        );
        this.setImage(image.as_deref());
        this.setImagePosition(NSCellImagePosition::ImageOnly);
        this.setBordered(false);
        this.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
        this.setFocusRingType(NSFocusRingType::None);
        this.setToolTip(Some(&NSString::from_str(text)));
        unsafe {
            this.setTarget(Some(&this));
            this.setAction(Some(sel!(showInfo:)));
        }
        sized(&this, Some(16.0), Some(16.0));
        this
    }
}

fn stack(
    mtm: MainThreadMarker,
    orientation: NSUserInterfaceLayoutOrientation,
    spacing: f64,
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(orientation);
    stack.setSpacing(spacing);
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);
    stack
}

pub fn vstack(mtm: MainThreadMarker, spacing: f64) -> Retained<NSStackView> {
    let stack = stack(mtm, NSUserInterfaceLayoutOrientation::Vertical, spacing);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack
}

pub fn hstack(mtm: MainThreadMarker, spacing: f64) -> Retained<NSStackView> {
    let stack = stack(mtm, NSUserInterfaceLayoutOrientation::Horizontal, spacing);
    stack.setAlignment(NSLayoutAttribute::CenterY);
    stack
}

pub fn insets(top: f64, left: f64, bottom: f64, right: f64) -> NSEdgeInsets {
    NSEdgeInsets { top, left, bottom, right }
}

pub fn fill_width(parent: &NSStackView, child: &NSView, inset: f64) {
    child.setTranslatesAutoresizingMaskIntoConstraints(false);
    let constraint = child.widthAnchor().constraintEqualToAnchor_constant(&parent.widthAnchor(), -inset);
    constraint.setActive(true);
}

pub fn sized(view: &NSView, width: Option<f64>, height: Option<f64>) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    if let Some(width) = width {
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
    }
    if let Some(height) = height {
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
    }
}

pub fn pin_edges(child: &NSView, parent: &NSView) {
    child.setTranslatesAutoresizingMaskIntoConstraints(false);
    let constraints = [
        child.topAnchor().constraintEqualToAnchor(&parent.topAnchor()),
        child.leadingAnchor().constraintEqualToAnchor(&parent.leadingAnchor()),
        child.trailingAnchor().constraintEqualToAnchor(&parent.trailingAnchor()),
        child.bottomAnchor().constraintEqualToAnchor(&parent.bottomAnchor()),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
}

pub fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setTranslatesAutoresizingMaskIntoConstraints(false);
    label
}

pub fn detail(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setTranslatesAutoresizingMaskIntoConstraints(false);
    label
}

pub fn value_label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let label = label(mtm, text);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(12.0, 0.0)));
    label
}

pub fn field(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let field = NSTextField::textFieldWithString(&NSString::from_str(text), mtm);
    sized(&field, Some(220.0), None);
    field
}

pub fn set_status_color(label: &NSTextField, error: bool) {
    let color = if error { NSColor::systemRedColor() } else { NSColor::secondaryLabelColor() };
    label.setTextColor(Some(&color));
}

fn text_column(
    mtm: MainThreadMarker,
    title: &str,
    info: Option<&str>,
    lines: &[&NSView],
) -> Retained<NSStackView> {
    let column = vstack(mtm, 2.0);
    let heading = hstack(mtm, 5.0);
    let title = label(mtm, title);
    title.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    heading.addArrangedSubview(&title);
    if let Some(info) = info {
        heading.addArrangedSubview(&InfoButton::new(mtm, info));
    }
    column.addArrangedSubview(&heading);
    for line in lines {
        column.addArrangedSubview(line);
    }
    column
}

pub fn padded(mtm: MainThreadMarker, content: &NSView) -> Retained<NSView> {
    padded_with(mtm, content, ROW_INSET_Y, ROW_MIN_HEIGHT)
}

pub fn padded_compact(mtm: MainThreadMarker, content: &NSView) -> Retained<NSView> {
    padded_with(mtm, content, COMPACT_INSET_Y, COMPACT_MIN_HEIGHT)
}

fn padded_with(mtm: MainThreadMarker, content: &NSView, inset_y: f64, min_height: f64) -> Retained<NSView> {
    let row = NSView::new(mtm);
    row.setTranslatesAutoresizingMaskIntoConstraints(false);
    row.addSubview(content);
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    let constraints = [
        content.topAnchor().constraintEqualToAnchor_constant(&row.topAnchor(), inset_y),
        content.bottomAnchor().constraintEqualToAnchor_constant(&row.bottomAnchor(), -inset_y),
        content.leadingAnchor().constraintEqualToAnchor_constant(&row.leadingAnchor(), ROW_INSET_X),
        content.trailingAnchor().constraintEqualToAnchor_constant(&row.trailingAnchor(), -ROW_INSET_X),
        row.heightAnchor().constraintGreaterThanOrEqualToConstant(min_height),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    row
}

pub fn row(
    mtm: MainThreadMarker,
    title: &str,
    info: Option<&str>,
    lines: &[&NSView],
    trailing: &[&NSView],
) -> Retained<NSView> {
    let content = hstack(mtm, 12.0);
    let column = text_column(mtm, title, info, lines);
    column.setContentHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultHigh,
        NSLayoutConstraintOrientation::Horizontal,
    );
    column.setContentCompressionResistancePriority_forOrientation(
        NSLayoutPriorityDefaultLow,
        NSLayoutConstraintOrientation::Horizontal,
    );
    content.addArrangedSubview(&column);
    if !trailing.is_empty() {
        let spacer = NSView::new(mtm);
        spacer.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
        spacer.setContentCompressionResistancePriority_forOrientation(
            1.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        content.addArrangedSubview(&spacer);
        let controls = hstack(mtm, 8.0);
        for view in trailing {
            controls.addArrangedSubview(view);
        }
        controls.setContentHuggingPriority_forOrientation(
            NSLayoutPriorityDefaultHigh,
            NSLayoutConstraintOrientation::Horizontal,
        );
        controls.setContentCompressionResistancePriority_forOrientation(
            NSLayoutPriorityDefaultHigh,
            NSLayoutConstraintOrientation::Horizontal,
        );
        content.addArrangedSubview(&controls);
    }
    padded(mtm, &content)
}

pub fn block_row(
    mtm: MainThreadMarker,
    title: &str,
    info: Option<&str>,
    lines: &[&NSView],
    content: &NSView,
) -> Retained<NSView> {
    let stack = vstack(mtm, 8.0);
    stack.addArrangedSubview(&text_column(mtm, title, info, lines));
    stack.addArrangedSubview(content);
    fill_width(&stack, content, 0.0);
    padded(mtm, &stack)
}

fn separator(mtm: MainThreadMarker) -> Retained<NSView> {
    let holder = NSView::new(mtm);
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    holder.addSubview(&line);
    line.setTranslatesAutoresizingMaskIntoConstraints(false);
    let constraints = [
        line.leadingAnchor().constraintEqualToAnchor_constant(&holder.leadingAnchor(), ROW_INSET_X),
        line.trailingAnchor().constraintEqualToAnchor_constant(&holder.trailingAnchor(), -ROW_INSET_X),
        line.centerYAnchor().constraintEqualToAnchor(&holder.centerYAnchor()),
        holder.heightAnchor().constraintEqualToConstant(1.0),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    holder
}

/// runde box mit hairline-trennern. gibt pro zeile einen wrapper zurück (zeile plus trenner), damit aufrufer eine zeile ausblenden können, ohne eine linie zu hinterlassen. nur zeilen nach der ersten ausblenden.
pub fn card(mtm: MainThreadMarker, rows: &[Retained<NSView>]) -> (Retained<Surface>, Vec<Retained<NSView>>) {
    let column = vstack(mtm, 0.0);
    column.setDetachesHiddenViews(true);
    let mut wrappers = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let wrapper = vstack(mtm, 0.0);
        if index > 0 {
            let separator = separator(mtm);
            wrapper.addArrangedSubview(&separator);
            fill_width(&wrapper, &separator, 0.0);
        }
        wrapper.addArrangedSubview(row);
        fill_width(&wrapper, row, 0.0);
        column.addArrangedSubview(&wrapper);
        fill_width(&column, &wrapper, 0.0);
        wrappers.push(Retained::into_super(wrapper));
    }

    let card = Surface::new(mtm, NSColor::quaternarySystemFillColor(), CARD_RADIUS, false);
    card.wrap(&column);
    (card, wrappers)
}

pub fn plain_card(mtm: MainThreadMarker, rows: &[Retained<NSView>]) -> Retained<Surface> {
    let column = vstack(mtm, 0.0);
    column.setDetachesHiddenViews(true);
    for row in rows {
        column.addArrangedSubview(row);
        fill_width(&column, row, 0.0);
    }
    let card = Surface::new(mtm, NSColor::quaternarySystemFillColor(), CARD_RADIUS, false);
    card.wrap(&column);
    card
}

pub fn section(
    mtm: MainThreadMarker,
    header: Option<&str>,
    rows: &[Retained<NSView>],
    footer: Option<&str>,
) -> Retained<NSView> {
    section_hideable(mtm, header, rows, footer).0
}

pub fn section_hideable(
    mtm: MainThreadMarker,
    header: Option<&str>,
    rows: &[Retained<NSView>],
    footer: Option<&str>,
) -> (Retained<NSView>, Vec<Retained<NSView>>) {
    let section = vstack(mtm, 8.0);
    if let Some(header) = header {
        let header = label(mtm, header);
        header.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        section.addArrangedSubview(&header);
        section.setCustomSpacing_afterView(10.0, &header);
    }
    let (card, wrappers) = card(mtm, rows);
    section.addArrangedSubview(&card);
    fill_width(&section, &card, 0.0);
    if let Some(footer) = footer {
        let footer = detail(mtm, footer);
        section.addArrangedSubview(&footer);
        fill_width(&section, &footer, ROW_INSET_X * 2.0);
    }
    (Retained::into_super(section), wrappers)
}

pub fn form(mtm: MainThreadMarker, sections: &[Retained<NSView>]) -> Retained<NSScrollView> {
    let content = vstack(mtm, 26.0);
    content.setEdgeInsets(insets(PAGE_INSET, PAGE_INSET, PAGE_INSET, PAGE_INSET));
    for section in sections {
        content.addArrangedSubview(section);
        fill_width(&content, section, PAGE_INSET * 2.0);
    }
    let document = FlippedView::new(mtm);
    document.addSubview(&content);
    pin_edges(&content, &document);

    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    scroll.setAutohidesScrollers(true);
    scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
    scroll.setDocumentView(Some(&document));
    let clip = scroll.contentView();
    let constraints = [
        document.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
        document.leadingAnchor().constraintEqualToAnchor(&clip.leadingAnchor()),
        document.trailingAnchor().constraintEqualToAnchor(&clip.trailingAnchor()),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    scroll
}

pub fn symbol_image(symbol: &str, description: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(description)),
    )
}

pub fn icon_tile(
    mtm: MainThreadMarker,
    symbol: &str,
    fill: Retained<NSColor>,
    side: f64,
    radius: f64,
) -> Retained<Surface> {
    let tile = Surface::new(mtm, fill, radius, false);
    sized(&tile, Some(side), Some(side));
    if let Some(image) = symbol_image(symbol, symbol) {
        let icon = NSImageView::imageViewWithImage(&image, mtm);
        icon.setContentTintColor(Some(&NSColor::whiteColor()));
        icon.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        icon.setTranslatesAutoresizingMaskIntoConstraints(false);
        tile.addSubview(&icon);
        let pad = side * 0.22;
        let constraints = [
            icon.topAnchor().constraintEqualToAnchor_constant(&tile.topAnchor(), pad),
            icon.bottomAnchor().constraintEqualToAnchor_constant(&tile.bottomAnchor(), -pad),
            icon.leadingAnchor().constraintEqualToAnchor_constant(&tile.leadingAnchor(), pad),
            icon.trailingAnchor().constraintEqualToAnchor_constant(&tile.trailingAnchor(), -pad),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    }
    tile
}

pub fn tag(mtm: MainThreadMarker, text: &str) -> Retained<Surface> {
    let tag = Surface::new(mtm, NSColor::quaternarySystemFillColor(), 4.0, false);
    let label = label(mtm, text);
    label.setFont(Some(&NSFont::systemFontOfSize_weight(9.5, 0.3)));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    tag.addSubview(&label);
    let constraints = [
        label.topAnchor().constraintEqualToAnchor_constant(&tag.topAnchor(), 2.0),
        label.bottomAnchor().constraintEqualToAnchor_constant(&tag.bottomAnchor(), -2.0),
        label.leadingAnchor().constraintEqualToAnchor_constant(&tag.leadingAnchor(), 5.0),
        label.trailingAnchor().constraintEqualToAnchor_constant(&tag.trailingAnchor(), -5.0),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    tag
}

pub fn type_badge(mtm: MainThreadMarker, symbol: &str, tooltip: &str) -> Retained<Surface> {
    let badge = Surface::new(mtm, NSColor::quaternarySystemFillColor(), 7.0, false);
    sized(&badge, Some(28.0), Some(22.0));
    if let Some(image) = symbol_image(symbol, tooltip) {
        let icon = NSImageView::imageViewWithImage(&image, mtm);
        icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        icon.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        icon.setTranslatesAutoresizingMaskIntoConstraints(false);
        badge.addSubview(&icon);
        let constraints = [
            icon.centerXAnchor().constraintEqualToAnchor(&badge.centerXAnchor()),
            icon.centerYAnchor().constraintEqualToAnchor(&badge.centerYAnchor()),
            icon.widthAnchor().constraintEqualToConstant(13.0),
            icon.heightAnchor().constraintEqualToConstant(13.0),
        ];
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    }
    badge.setToolTip(Some(&NSString::from_str(tooltip)));
    badge
}

pub fn symbol_button(
    mtm: MainThreadMarker,
    symbol: &str,
    tooltip: &str,
    action: &Action,
) -> Retained<NSButton> {
    let button = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(""), None, None, mtm) };
    action.attach(&button);
    button.setBordered(false);
    button.setImagePosition(NSCellImagePosition::ImageOnly);
    button.setFocusRingType(NSFocusRingType::None);
    sized(&button, Some(26.0), Some(26.0));
    set_symbol(&button, symbol, tooltip, &NSColor::secondaryLabelColor());
    button
}

pub fn set_symbol(button: &NSButton, symbol: &str, tooltip: &str, tint: &NSColor) {
    button.setImage(symbol_image(symbol, tooltip).as_deref());
    button.setContentTintColor(Some(tint));
    button.setToolTip(Some(&NSString::from_str(tooltip)));
}

pub fn set_star(button: &NSButton, on: bool) {
    if on {
        set_symbol(button, "star.fill", "Default model", &NSColor::systemYellowColor());
    } else {
        set_symbol(button, "star", "Use as default", &NSColor::tertiaryLabelColor());
    }
}

pub fn search_field(mtm: MainThreadMarker, placeholder: &str, action: &Action) -> Retained<NSSearchField> {
    let field = NSSearchField::new(mtm);
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    field.setSendsSearchStringImmediately(true);
    action.attach(&field);
    field.setTranslatesAutoresizingMaskIntoConstraints(false);
    field
}

pub fn button(mtm: MainThreadMarker, title: &str, action: &Action) -> Retained<NSButton> {
    let button =
        unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm) };
    action.attach(&button);
    button.setTranslatesAutoresizingMaskIntoConstraints(false);
    button
}

pub fn toggle(mtm: MainThreadMarker, on: bool, action: &Action) -> Retained<NSSwitch> {
    let toggle = NSSwitch::new(mtm);
    action.attach(&toggle);
    toggle.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    toggle.setTranslatesAutoresizingMaskIntoConstraints(false);
    toggle
}

pub fn popup(
    mtm: MainThreadMarker,
    items: &[&str],
    selected: usize,
    action: &Action,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for item in items {
        popup.addItemWithTitle(&NSString::from_str(item));
    }
    if !items.is_empty() {
        popup.selectItemAtIndex(selected as isize);
    }
    action.attach(&popup);
    popup.setTranslatesAutoresizingMaskIntoConstraints(false);
    popup
}

pub fn segmented(
    mtm: MainThreadMarker,
    items: &[&str],
    selected: usize,
    action: &Action,
) -> Retained<NSSegmentedControl> {
    let control = NSSegmentedControl::new(mtm);
    control.setSegmentCount(items.len() as isize);
    for (index, item) in items.iter().enumerate() {
        control.setLabel_forSegment(&NSString::from_str(item), index as isize);
        control.setWidth_forSegment(78.0, index as isize);
    }
    control.setSelectedSegment(selected as isize);
    action.attach(&control);
    control.setTranslatesAutoresizingMaskIntoConstraints(false);
    control
}

pub fn slider(
    mtm: MainThreadMarker,
    range: std::ops::RangeInclusive<f64>,
    value: f64,
    action: &Action,
) -> Retained<NSSlider> {
    let slider = NSSlider::new(mtm);
    slider.setMinValue(*range.start());
    slider.setMaxValue(*range.end());
    slider.setDoubleValue(value);
    slider.setContinuous(false);
    action.attach(&slider);
    sized(&slider, Some(170.0), None);
    slider
}

pub fn progress(mtm: MainThreadMarker) -> Retained<NSProgressIndicator> {
    let progress = NSProgressIndicator::new(mtm);
    progress.setStyle(NSProgressIndicatorStyle::Bar);
    progress.setIndeterminate(false);
    progress.setMinValue(0.0);
    progress.setMaxValue(1.0);
    progress.setDoubleValue(0.0);
    progress.setTranslatesAutoresizingMaskIntoConstraints(false);
    progress
}

pub fn text_area(mtm: MainThreadMarker, height: f64) -> (Retained<NSScrollView>, Retained<NSTextView>) {
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(true);
    scroll.setBorderType(objc2_app_kit::NSBorderType::BezelBorder);
    let view = NSTextView::new(mtm);
    view.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11.0, 0.0)));
    view.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    scroll.setDocumentView(Some(&view));
    sized(&scroll, None, Some(height));
    (scroll, view)
}

pub struct RecorderIvars {
    label: Retained<NSTextField>,
    current: Cell<TriggerKey>,
    recording: Cell<bool>,
    paused: Arc<AtomicBool>,
    monitor: RefCell<Option<Retained<AnyObject>>>,
    capture: RefCell<Option<CaptureHandler>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "TfShortcutRecorder"]
    #[ivars = RecorderIvars]
    pub struct ShortcutRecorder;

    unsafe impl NSObjectProtocol for ShortcutRecorder {}

    impl ShortcutRecorder {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            self.begin();
        }

        #[unsafe(method(startRecording:))]
        fn start_recording(&self, _sender: Option<&AnyObject>) {
            self.begin();
        }
    }
);

impl ShortcutRecorder {
    pub fn new(
        mtm: MainThreadMarker,
        current: TriggerKey,
        paused: Arc<AtomicBool>,
        on_capture: impl Fn(TriggerKey) + 'static,
    ) -> Retained<Self> {
        let rect = NSRect::new(NSPoint::ZERO, NSSize::new(RECORDER_WIDTH, RECORDER_HEIGHT));
        let label = NSTextField::labelWithString(&NSString::from_str(&current.display()), mtm);
        label.setFrame(NSRect::new(NSPoint::new(8.0, 4.0), NSSize::new(RECORDER_WIDTH - 16.0, 18.0)));
        let view = Self::alloc(mtm).set_ivars(RecorderIvars {
            label,
            current: Cell::new(current),
            recording: Cell::new(false),
            paused,
            monitor: RefCell::new(None),
            capture: RefCell::new(Some(Box::new(on_capture))),
        });
        let view: Retained<Self> = unsafe { msg_send![super(view), initWithFrame: rect] };
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            layer.setCornerRadius(5.0);
            layer.setBorderWidth(1.0);
            let color = objc2_app_kit::NSColor::separatorColor().CGColor();
            layer.setBorderColor(Some(&color));
        }
        let label = view.ivars().label.clone();
        view.addSubview(&label);
        sized(&view, Some(RECORDER_WIDTH), Some(RECORDER_HEIGHT));
        let target: &AnyObject = unsafe { &*((&*view) as *const Self).cast::<AnyObject>() };
        let overlay = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(""),
                Some(target),
                Some(sel!(startRecording:)),
                mtm,
            )
        };
        overlay.setBordered(false);
        overlay.setFrame(NSRect::new(NSPoint::ZERO, rect.size));
        view.addSubview(&overlay);
        view
    }

    pub fn set_current(&self, key: TriggerKey) {
        self.ivars().current.set(key);
        if !self.ivars().recording.get() {
            self.ivars().label.setStringValue(&NSString::from_str(&key.display()));
        }
    }

    pub fn cancel_recording(&self) {
        if self.ivars().recording.get() {
            self.cancel();
        }
    }

    fn begin(&self) {
        if self.ivars().recording.get() {
            return;
        }
        self.ivars().recording.set(true);
        self.ivars().paused.store(true, Ordering::Relaxed);
        self.ivars().label.setStringValue(&NSString::from_str("Press a shortcut…"));

        let ptr = self as *const Self;
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: das settings-fenster hält den recorder, solange der monitor existiert.
            let recorder = unsafe { &*ptr };
            recorder.handle(unsafe { event.as_ref() })
        });
        let mask = NSEventMask::KeyDown | NSEventMask::FlagsChanged | NSEventMask::SystemDefined;
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        *self.ivars().monitor.borrow_mut() = monitor;
    }

    fn handle(&self, event: &NSEvent) -> *mut NSEvent {
        let null = std::ptr::null_mut();
        let flags = event.modifierFlags();
        let relevant = flags.intersection(
            NSEventModifierFlags::Shift
                | NSEventModifierFlags::Control
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Command,
        );

        match event.r#type() {
            NSEventType::FlagsChanged => {
                let keycode = event.keyCode();
                // manche tastaturen melden eine funktionstaste als flags-change mit fn-flag.
                if is_function_key(keycode) {
                    if flags.contains(NSEventModifierFlags::Function) {
                        self.finish(TriggerKey::Chord {
                            keycode,
                            shift: relevant.contains(NSEventModifierFlags::Shift),
                            control: relevant.contains(NSEventModifierFlags::Control),
                            option: relevant.contains(NSEventModifierFlags::Option),
                            command: relevant.contains(NSEventModifierFlags::Command),
                        });
                    }
                    return null;
                }
                let candidate = match keycode {
                    61 => Some(TriggerKey::RightOption),
                    54 => Some(TriggerKey::RightCommand),
                    62 => Some(TriggerKey::RightControl),
                    60 => Some(TriggerKey::RightShift),
                    _ => None,
                };
                let Some(candidate) = candidate else {
                    return null;
                };
                let wanted = match candidate {
                    TriggerKey::RightOption => NSEventModifierFlags::Option,
                    TriggerKey::RightCommand => NSEventModifierFlags::Command,
                    TriggerKey::RightControl => NSEventModifierFlags::Control,
                    TriggerKey::RightShift => NSEventModifierFlags::Shift,
                    TriggerKey::Chord { .. } => unreachable!(),
                };
                if relevant == wanted {
                    self.finish(candidate);
                }
                null
            }
            NSEventType::KeyDown => {
                let keycode = event.keyCode();
                if keycode == 53 {
                    self.cancel();
                    return null;
                }
                if event.isARepeat() || (relevant.is_empty() && !is_function_key(keycode)) {
                    return null;
                }
                self.finish(TriggerKey::Chord {
                    keycode,
                    shift: relevant.contains(NSEventModifierFlags::Shift),
                    control: relevant.contains(NSEventModifierFlags::Control),
                    option: relevant.contains(NSEventModifierFlags::Option),
                    command: relevant.contains(NSEventModifierFlags::Command),
                });
                null
            }
            NSEventType::SystemDefined => {
                if event.subtype().0 != 8 {
                    return event as *const NSEvent as *mut NSEvent;
                }
                let Some(press) = media_function_key(event.data1() as i64) else {
                    return event as *const NSEvent as *mut NSEvent;
                };
                if press.down && !press.repeat {
                    self.finish(TriggerKey::Chord {
                        keycode: press.keycode,
                        shift: relevant.contains(NSEventModifierFlags::Shift),
                        control: relevant.contains(NSEventModifierFlags::Control),
                        option: relevant.contains(NSEventModifierFlags::Option),
                        command: relevant.contains(NSEventModifierFlags::Command),
                    });
                }
                null
            }
            _ => null,
        }
    }

    fn finish(&self, key: TriggerKey) {
        self.stop_monitor();
        self.ivars().recording.set(false);
        self.ivars().current.set(key);
        self.ivars().label.setStringValue(&NSString::from_str(&key.display()));
        if let Some(capture) = self.ivars().capture.borrow().as_ref() {
            capture(key);
        }
    }

    fn cancel(&self) {
        self.stop_monitor();
        self.ivars().recording.set(false);
        self.ivars().label.setStringValue(&NSString::from_str(&self.ivars().current.get().display()));
    }

    fn stop_monitor(&self) {
        if let Some(monitor) = self.ivars().monitor.borrow_mut().take() {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        self.ivars().paused.store(false, Ordering::Relaxed);
    }
}
