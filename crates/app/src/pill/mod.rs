mod bars;

use std::cell::Cell;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSPanel, NSScreen, NSStatusWindowLevel, NSView,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CAGradientLayer, CALayer, CATextLayer, CATransaction, kCAGradientLayerConic};

pub use bars::Animation;

const WIDTH: f64 = 64.0;
const HEIGHT: f64 = 34.0;
const BORDER: f64 = 1.0;
const GLOW_WIDTH: f64 = 2.5;
/// sekunden, die das glühen für eine runde um die kapsel braucht.
const GLOW_PERIOD: f64 = 1.4;
const BAR_WIDTH: f64 = 3.0;
const BAR_GAP: f64 = 3.5;
const BAR_MAX: f64 = 18.0;
const BOTTOM_MARGIN: f64 = 18.0;
const LABEL_FONT: f64 = 11.0;
/// höhere werte lassen die kapsel schneller einpendeln; die breite läuft exponentiell aus.
const EXPAND_RATE: f64 = 14.0;

pub struct Pill {
    panel: Retained<NSPanel>,
    capsule: Retained<CALayer>,
    bars: Vec<Retained<CALayer>>,
    label: Retained<CATextLayer>,
    glow: Retained<CAGradientLayer>,
    glow_mask: Retained<CALayer>,
    glow_enabled: Cell<bool>,
    width: Cell<f64>,
    target_width: Cell<f64>,
    label_span: Cell<f64>,
    last_step: Cell<Instant>,
    anchor: Cell<NSPoint>,
}

impl Pill {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setIgnoresMouseEvents(true);
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
        view.setWantsLayer(true);
        let capsule = view.layer().expect("layer-backed view");
        capsule.setBackgroundColor(Some(&NSColor::blackColor().CGColor()));
        capsule.setCornerRadius(HEIGHT / 2.0);
        capsule.setBorderWidth(BORDER);
        capsule.setMasksToBounds(true);

        let white = NSColor::whiteColor().CGColor();
        let bars = (0..bars::COUNT)
            .map(|_| {
                let bar = CALayer::new();
                bar.setBackgroundColor(Some(&white));
                bar.setCornerRadius(BAR_WIDTH / 2.0);
                capsule.addSublayer(&bar);
                bar
            })
            .collect();

        let label = CATextLayer::new();
        label.setFontSize(LABEL_FONT);
        label.setForegroundColor(Some(&white));
        label.setHidden(true);
        label.setOpacity(0.0);
        capsule.addSublayer(&label);

        let glow = CAGradientLayer::new();
        glow.setType(unsafe { kCAGradientLayerConic });
        glow.setHidden(true);
        let glow_mask = CALayer::new();
        glow_mask.setBorderWidth(GLOW_WIDTH);
        glow_mask.setBorderColor(Some(&NSColor::blackColor().CGColor()));
        glow_mask.setCornerRadius(HEIGHT / 2.0);
        // SAFETY: der glow-layer hält die mask-layer am leben.
        unsafe { glow.setMask(Some(&glow_mask)) };
        capsule.addSublayer(&glow);
        panel.setContentView(Some(&view));

        let pill = Self {
            panel,
            capsule,
            bars,
            label,
            glow,
            glow_mask,
            glow_enabled: Cell::new(false),
            width: Cell::new(WIDTH),
            target_width: Cell::new(WIDTH),
            label_span: Cell::new(0.0),
            last_step: Cell::new(Instant::now()),
            anchor: Cell::new(NSPoint::ZERO),
        };
        pill.render(Animation::Listening { level: 0.0 }, 0.0);
        pill
    }

    pub fn show(&self, mtm: MainThreadMarker) {
        if let Some(anchor) = bottom_center_of_active_screen(mtm) {
            self.anchor.set(anchor);
            self.apply_frame(0.0);
        }
        self.panel.orderFrontRegardless();
    }

    pub fn show_mode(&self, name: &str) {
        let span = (name.chars().count() as f64 * 6.4 + 22.0).clamp(40.0, 170.0);
        self.label_span.set(span);
        self.target_width.set(WIDTH + span);
        let text = NSString::from_str(name);
        // SAFETY: catextlayer kopiert den übergebenen string.
        let object: &objc2::runtime::AnyObject =
            unsafe { &*(Retained::as_ptr(&text) as *const objc2::runtime::AnyObject) };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        unsafe { self.label.setString(Some(object)) };
        self.label.setHidden(false);
        self.label.setFrame(NSRect::new(
            NSPoint::new(WIDTH + 4.0, (HEIGHT - 16.0) / 2.0),
            NSSize::new(span - 8.0, 16.0),
        ));
        CATransaction::commit();
    }

    pub fn clear_mode(&self) {
        self.target_width.set(WIDTH);
    }

    pub fn reset_mode(&self) {
        self.target_width.set(WIDTH);
        self.width.set(WIDTH);
        self.update_label();
    }

    /// rückt die breiten-animation vor; einmal pro frame aufrufen.
    pub fn step(&self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step.replace(now)).as_secs_f64().min(0.1);
        let (width, target) = (self.width.get(), self.target_width.get());
        if width == target {
            return;
        }
        let next = width + (target - width) * (1.0 - (-EXPAND_RATE * dt).exp());
        self.width.set(if (target - next).abs() < 0.5 { target } else { next });
        self.apply_frame(0.0);
        self.update_label();
    }

    fn update_label(&self) {
        let span = self.label_span.get();
        let opacity = if span > 0.0 { ((self.width.get() - WIDTH) / span).clamp(0.0, 1.0) } else { 0.0 };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.label.setOpacity(opacity as f32);
        if self.width.get() == WIDTH {
            self.label.setHidden(true);
        }
        CATransaction::commit();
    }

    fn apply_frame(&self, dx: f64) {
        let width = self.width.get();
        let anchor = self.anchor.get();
        let origin = NSPoint::new(anchor.x - width / 2.0 + dx, anchor.y);
        self.panel.setFrame_display(NSRect::new(origin, NSSize::new(width, HEIGHT)), true);
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(width, HEIGHT));
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.glow.setFrame(bounds);
        self.glow_mask.setFrame(bounds);
        CATransaction::commit();
    }

    /// konfiguriert das glühen bei `Processing`; `color` ist `#RRGGBB`.
    pub fn set_glow(&self, enabled: bool, color: &str) {
        self.glow_enabled.set(enabled);
        let (r, g, b) = parse_hex(color).unwrap_or((0.31, 0.55, 1.0));
        let solid = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).CGColor();
        let clear = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 0.0).CGColor();
        let colors = [&*clear, &*clear, &*solid, &*clear].map(|c| {
            // SAFETY: cgcolor ist toll-free bridged zu einem objective-c-objekt.
            unsafe { &*(c as *const _ as *const objc2::runtime::AnyObject) }
        });
        let locations = NSArray::from_retained_slice(&[0.0, 0.45, 0.95, 1.0].map(|v| NSNumber::new_f64(v)));
        // SAFETY: das array enthält cgcolor-objekte, wie cagradientlayer erwartet.
        unsafe { self.glow.setColors(Some(&NSArray::from_slice(&colors))) };
        self.glow.setLocations(Some(&locations));
    }

    fn update_glow(&self, active: bool, t: f64) {
        let show = active && self.glow_enabled.get();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.glow.setHidden(!show);
        if show {
            let angle = t / GLOW_PERIOD * std::f64::consts::TAU;
            self.glow.setStartPoint(NSPoint::new(0.5, 0.5));
            self.glow.setEndPoint(NSPoint::new(0.5 + 0.5 * angle.cos(), 0.5 - 0.5 * angle.sin()));
        }
        CATransaction::commit();
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    pub fn set_alpha(&self, alpha: f64) {
        self.panel.setAlphaValue(alpha.clamp(0.0, 1.0));
    }

    pub fn shake(&self, dx: f64) {
        self.apply_frame(dx);
    }

    pub fn render(&self, animation: Animation, t: f64) {
        let heights = bars::heights(animation, t, BAR_WIDTH, BAR_MAX);
        let total = bars::COUNT as f64 * BAR_WIDTH + (bars::COUNT - 1) as f64 * BAR_GAP;
        let left = (WIDTH - total) / 2.0;

        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.update_glow(matches!(animation, Animation::Processing), t);
        let border = match animation {
            Animation::Error => NSColor::systemRedColor(),
            _ => NSColor::colorWithWhite_alpha(0.32, 1.0),
        };
        self.capsule.setBorderColor(Some(&border.CGColor()));
        for (i, (bar, h)) in self.bars.iter().zip(heights).enumerate() {
            let x = left + i as f64 * (BAR_WIDTH + BAR_GAP);
            bar.setFrame(NSRect::new(NSPoint::new(x, (HEIGHT - h) / 2.0), NSSize::new(BAR_WIDTH, h)));
        }
        CATransaction::commit();
    }
}

fn bottom_center_of_active_screen(mtm: MainThreadMarker) -> Option<NSPoint> {
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let screen = screens
        .iter()
        .find(|s| {
            let f = s.frame();
            (f.origin.x..f.origin.x + f.size.width).contains(&mouse.x)
                && (f.origin.y..f.origin.y + f.size.height).contains(&mouse.y)
        })
        .or_else(|| screens.firstObject())?;
    let visible = screen.visibleFrame();
    Some(NSPoint::new(visible.origin.x + visible.size.width / 2.0, visible.origin.y + BOTTOM_MARGIN))
}

fn parse_hex(value: &str) -> Option<(f64, f64, f64)> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| f64::from(v) / 255.0);
    Some((channel(0)?, channel(2)?, channel(4)?))
}
