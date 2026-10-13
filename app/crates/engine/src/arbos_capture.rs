//! `screenshot.capture` — the frontmost Arbos project window as a PNG under
//! the project context.
//!
//! The pixels come from the headed app, which owns the window: it registers
//! [`set_window_capture`] at boot with the Appshot capture path, so the op
//! runs in the window's own process and the capture needs no Screen Recording
//! permission. A headless or remote engine owns no window, registers no
//! capture, and the op says so in plain words.

use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use serde::Serialize;

/// The PNG bytes of one capture, plus the title of the window they show.
#[derive(Debug, Clone)]
pub struct CapturedWindow {
    pub png: Vec<u8>,
    pub window_title: Option<String>,
}

/// One capture of the app's own frontmost project window.
pub type CaptureWindow = Arc<dyn Fn() -> Result<CapturedWindow, String> + Send + Sync>;

static WINDOW_CAPTURE: OnceLock<CaptureWindow> = OnceLock::new();

/// The headed app hands the engine it embeds the Appshot capture path for its
/// own window. Registering twice keeps the first capture; the app boots once.
pub fn set_window_capture(capture: CaptureWindow) {
    let _ = WINDOW_CAPTURE.set(capture);
}

/// The registered capture, when this process owns the project window.
pub fn window_capture() -> Option<CaptureWindow> {
    WINDOW_CAPTURE.get().cloned()
}

/// Where captures land, under the project context.
pub const SCREENSHOT_DIR: &str = "screenshots";
/// The largest PNG the op writes, the same budget an Appshot attachment has.
pub const MAX_SCREENSHOT_BYTES: usize = 24 * 1024 * 1024;

/// `screenshot.capture`'s reply.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotView {
    /// The saved path, relative to the project context.
    pub path: String,
    /// The PNG's size in bytes.
    pub bytes: usize,
    /// The captured window's title, when the app reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
}

/// The op behind `screenshot.capture`: run `capture` — the hook the headed app
/// registered, or `None` in a headless or remote engine — and write the PNG it
/// returns under `.arbos/context/screenshots/`.
pub fn capture_screenshot(
    project: &Path,
    now: DateTime<Utc>,
    capture: Option<CaptureWindow>,
) -> Result<ScreenshotView, String> {
    let capture = capture.ok_or_else(|| {
        "Arbos's project window is not available to capture here. Open the project in the Arbos app and try again."
            .to_owned()
    })?;
    let window = capture()?;
    if !is_png(&window.png) {
        return Err("the capture did not produce a PNG picture".into());
    }
    if window.png.len() > MAX_SCREENSHOT_BYTES {
        return Err("the captured window is larger than Arbos's 24 MB picture limit.".into());
    }
    let name = screenshot_file_name(now);
    let relative = format!("{SCREENSHOT_DIR}/{name}");
    let path = crate::arbos_layout::context_file(project, &relative)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Arbos could not save the picture: {err}"))?;
    }
    fs::write(&path, &window.png)
        .map_err(|err| format!("Arbos could not save the picture: {err}"))?;
    Ok(ScreenshotView {
        path: relative,
        bytes: window.png.len(),
        window_title: window.window_title,
    })
}

/// `screenshot-YYYYMMDD-HHMMSS-<short id>.png`: readable in a list, ordered by
/// capture time, and never two captures in one name.
pub fn screenshot_file_name(now: DateTime<Utc>) -> String {
    format!(
        "screenshot-{}-{}.png",
        now.format("%Y%m%d-%H%M%S"),
        &crate::new_id()[..8],
    )
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
}
