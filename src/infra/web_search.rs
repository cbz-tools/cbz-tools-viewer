use std::path::PathBuf;
use std::process::Command;

use crate::domain::app_settings::{
    AppSettings, WebSearchBrowser, WebSearchEntry, WebSearchOpenMode,
};

#[derive(Clone, Debug)]
pub struct WebSearchMenuItem {
    pub search_index: usize,
    pub display: String,
}

pub fn menu_items(entries: &[WebSearchEntry]) -> Vec<WebSearchMenuItem> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| is_valid_entry(entry))
        .map(|(search_index, entry)| WebSearchMenuItem {
            search_index,
            display: entry.display.clone(),
        })
        .collect()
}

pub fn launch(settings: &AppSettings, search_index: usize, token: &str) {
    let Some(entry) = settings.web_searches.get(search_index) else {
        tracing::warn!(search_index, "web search entry index is out of range");
        return;
    };
    let Some(url) = build_url(&entry.link, token) else {
        tracing::warn!(search_index, "web search entry is invalid");
        return;
    };
    let Some(executable) = browser_executable(settings.web_search_browser) else {
        tracing::warn!(browser = ?settings.web_search_browser, "web search browser executable not found");
        return;
    };
    let args = browser_args(
        settings.web_search_browser,
        settings.web_search_open_mode,
        &url,
    );
    if let Err(error) = Command::new(&executable).args(args).spawn() {
        tracing::warn!(
            browser = ?settings.web_search_browser,
            executable = %executable.display(),
            %error,
            "failed to launch web search browser"
        );
    }
}

pub fn is_valid_entry(entry: &WebSearchEntry) -> bool {
    !entry.display.trim().is_empty() && !entry.link.trim().is_empty() && entry.link.contains("%s")
}

pub fn build_url(link: &str, token: &str) -> Option<String> {
    let link = link.trim();
    if !link.contains("%s") || !has_http_scheme(link) {
        return None;
    }
    Some(link.replace("%s", &percent_encode(token)))
}

fn has_http_scheme(link: &str) -> bool {
    let Some((scheme, rest)) = link.split_once("://") else {
        return false;
    };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !rest.is_empty()
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(*byte >> 4) as usize] as char);
            encoded.push(HEX[(*byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn browser_args(browser: WebSearchBrowser, open_mode: WebSearchOpenMode, url: &str) -> Vec<String> {
    let mut args = Vec::with_capacity(2);
    match (browser, open_mode) {
        (WebSearchBrowser::Chrome | WebSearchBrowser::Edge, WebSearchOpenMode::Tab) => {}
        (WebSearchBrowser::Chrome | WebSearchBrowser::Edge, WebSearchOpenMode::NewWindow) => {
            args.push("--new-window".to_owned());
        }
        (WebSearchBrowser::Firefox, WebSearchOpenMode::Tab) => args.push("--new-tab".to_owned()),
        (WebSearchBrowser::Firefox, WebSearchOpenMode::NewWindow) => {
            args.push("--new-window".to_owned());
        }
    }
    args.push(url.to_owned());
    args
}

fn browser_executable(browser: WebSearchBrowser) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let mut candidates = Vec::new();
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let program_files_x86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
        match browser {
            WebSearchBrowser::Chrome => {
                push_candidate(
                    &mut candidates,
                    program_files,
                    ["Google", "Chrome", "Application", "chrome.exe"],
                );
                push_candidate(
                    &mut candidates,
                    program_files_x86,
                    ["Google", "Chrome", "Application", "chrome.exe"],
                );
            }
            WebSearchBrowser::Edge => {
                push_candidate(
                    &mut candidates,
                    program_files_x86,
                    ["Microsoft", "Edge", "Application", "msedge.exe"],
                );
                push_candidate(
                    &mut candidates,
                    program_files,
                    ["Microsoft", "Edge", "Application", "msedge.exe"],
                );
            }
            WebSearchBrowser::Firefox => {
                push_candidate(
                    &mut candidates,
                    program_files,
                    ["Mozilla Firefox", "firefox.exe"],
                );
                push_candidate(
                    &mut candidates,
                    program_files_x86,
                    ["Mozilla Firefox", "firefox.exe"],
                );
            }
        }
        candidates.into_iter().find(|path| path.is_file())
    }
    #[cfg(not(windows))]
    {
        let _ = browser;
        None
    }
}

#[cfg(windows)]
fn push_candidate<const N: usize>(
    candidates: &mut Vec<PathBuf>,
    root: Option<PathBuf>,
    parts: [&str; N],
) {
    if let Some(root) = root {
        candidates.push(parts.into_iter().fold(root, |path, part| path.join(part)));
    }
}
