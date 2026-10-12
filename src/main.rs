// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
rust_i18n::i18n!("locales", fallback = "en");

mod actions;
mod cleaners;
mod format;
mod history;
mod mcp;
mod platform;
mod rules;
mod scan;
mod settings;
mod state;
mod treemap;
mod ui;

fn main() {
    let (tx, rx) = async_channel::bounded(1);
    let app = state::Shared::new(platform::settings_path(), tx);
    let (language, mcp) = {
        let c = app.core.lock().unwrap();
        (c.settings.language.clone(), c.settings.mcp_enabled.then_some(c.settings.mcp_port))
    };
    ui::apply_language(language.as_deref());
    if let Some(port) = mcp {
        mcp::spawn(app.clone(), port);
    }
    ui::run(app, rx);
}
