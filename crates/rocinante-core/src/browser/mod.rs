//! The browser: Rocinante opens this project's pages in its own Chrome
//! window (a separate "Rocinante" profile) and drives it over the Chrome
//! DevTools Protocol — no extension. Models look with screenshots (vision
//! models) or a text snapshot (any model), and use the page with clicks,
//! typing, keys and scrolling. Rocinante keeps the page on the project's
//! own site.

mod cdp;
pub mod chrome;
pub mod guard;
mod page;
mod tool;

pub use tool::BrowserTool;

/// The browser tool for this config, when it's enabled and a Chrome-family
/// browser is installed.
pub fn tool(config: &crate::config::Config) -> Option<BrowserTool> {
    if !config.browser.enabled {
        return None;
    }
    let exe = chrome::find(config.browser.path.as_deref())?;
    Some(BrowserTool::new(exe, config.browser.headless))
}
