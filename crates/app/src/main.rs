mod app;
mod chat;
mod history;
mod hotkey;
mod insert;
mod live;
mod pill;
mod settings;
mod shell;
mod ui;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let _ = tf_core::log::init(&tf_core::paths::log_path());
    tf_core::log_info!("tf {} starting", env!("CARGO_PKG_VERSION"));
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        tf_core::log_error!("panic on {}: {info}", thread.name().unwrap_or("unnamed"));
    }));

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    if app::activate_existing() {
        return;
    }

    let delegate = app::AppDelegate::new(mtm);
    delegate.install(&app);
    delegate.apply_dock_policy();
    app.run();
}
