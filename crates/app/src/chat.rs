
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSFont, NSLayoutConstraint, NSStackView, NSTextField, NSTitlePosition, NSView,
};
use objc2_foundation::{NSArray, NSSize, NSString};

use crate::ui;

const BODY_MARGIN: NSSize = NSSize::new(12.0, 8.0);
const GAP: f64 = 3.0;
pub const TIGHT_GAP: f64 = 3.0;

pub struct Bubble<'a> {
    pub is_me: bool,
    pub name: &'a str,
    pub name_color: Retained<NSColor>,
    pub time: &'a str,
    pub text: &'a str,
    pub max_width: f64,
    /// folgebubble vom selben sprecher: ohne name und zeit darüber.
    pub continued: bool,
    pub controls: Vec<Retained<NSView>>,
}

pub fn conversation(mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = ui::vstack(mtm, 10.0);
    stack.setEdgeInsets(ui::insets(14.0, 14.0, 14.0, 14.0));
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);
    stack
}

pub fn separator(mtm: MainThreadMarker, text: &str) -> Retained<NSView> {
    let label = ui::detail(mtm, text);
    label.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    Retained::into_super(Retained::into_super(label))
}

pub fn push(stack: &NSStackView, row: &NSView) {
    stack.addArrangedSubview(row);
    ui::fill_width(stack, row, 28.0);
}

pub fn row(mtm: MainThreadMarker, bubble: Bubble) -> Retained<NSView> {
    let row = NSView::new(mtm);

    let header = ui::hstack(mtm, 6.0);
    if !bubble.is_me && !bubble.continued {
        let name = ui::detail(mtm, bubble.name);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
        name.setTextColor(Some(&bubble.name_color));
        header.addArrangedSubview(&name);
    }
    if !bubble.continued {
        let time = ui::detail(mtm, bubble.time);
        time.setTextColor(Some(&NSColor::tertiaryLabelColor()));
        header.addArrangedSubview(&time);
    }
    for control in &bubble.controls {
        header.addArrangedSubview(control);
    }

    let text = NSTextField::wrappingLabelWithString(&NSString::from_str(bubble.text), mtm);
    text.setMaximumNumberOfLines(0);
    text.setPreferredMaxLayoutWidth(bubble.max_width);
    text.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    text.setSelectable(true);
    text.setTextColor(Some(&*if bubble.is_me { NSColor::whiteColor() } else { NSColor::labelColor() }));

    let body = NSBox::new(mtm);
    body.setBoxType(NSBoxType::Custom);
    body.setTitlePosition(NSTitlePosition::NoTitle);
    body.setBorderWidth(0.0);
    body.setCornerRadius(17.0);
    body.setFillColor(&*if bubble.is_me {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.16, 0.58, 1.0, 1.0)
    } else {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.23, 0.23, 0.24, 1.0)
    });
    text.setTranslatesAutoresizingMaskIntoConstraints(false);
    body.addSubview(&text);

    for part in [&header as &NSView, &body] {
        part.setTranslatesAutoresizingMaskIntoConstraints(false);
        row.addSubview(part);
    }
    let mut constraints = vec![
        body.bottomAnchor().constraintEqualToAnchor(&row.bottomAnchor()),
        text.topAnchor().constraintEqualToAnchor_constant(&body.topAnchor(), BODY_MARGIN.height),
        text.bottomAnchor().constraintEqualToAnchor_constant(&body.bottomAnchor(), -BODY_MARGIN.height),
        text.leadingAnchor().constraintEqualToAnchor_constant(&body.leadingAnchor(), BODY_MARGIN.width),
        text.trailingAnchor().constraintEqualToAnchor_constant(&body.trailingAnchor(), -BODY_MARGIN.width),
    ];
    if bubble.continued {
        header.setHidden(true);
        constraints.push(body.topAnchor().constraintEqualToAnchor(&row.topAnchor()));
    } else {
        constraints.push(header.topAnchor().constraintEqualToAnchor(&row.topAnchor()));
        constraints.push(body.topAnchor().constraintEqualToAnchor_constant(&header.bottomAnchor(), GAP));
    }
    if bubble.is_me {
        constraints
            .push(header.trailingAnchor().constraintEqualToAnchor_constant(&body.trailingAnchor(), -6.0));
        constraints.push(body.trailingAnchor().constraintEqualToAnchor(&row.trailingAnchor()));
        constraints.push(body.leadingAnchor().constraintGreaterThanOrEqualToAnchor(&row.leadingAnchor()));
    } else {
        constraints.push(header.leadingAnchor().constraintEqualToAnchor_constant(&body.leadingAnchor(), 6.0));
        constraints.push(body.leadingAnchor().constraintEqualToAnchor(&row.leadingAnchor()));
        constraints.push(body.trailingAnchor().constraintLessThanOrEqualToAnchor(&row.trailingAnchor()));
    }
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    row
}
