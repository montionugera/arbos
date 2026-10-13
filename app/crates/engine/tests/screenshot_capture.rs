//! `screenshot.capture` from the tool's side: the capture hook the headed app
//! registers, the PNG the engine writes under the project context, and the
//! path it returns so Main can tell the person and open it.
//!
//! The pixels themselves are stubbed at the boundary — the same seam the op
//! uses, where the headed app hands the engine its Appshot capture path. The
//! unit tests next to `arbos_capture` pin the file name; this file pins the
//! wiring the layers share: the hook, the path resolution, the bytes on disk,
//! and the plain-word refusals when no window can be captured.

use std::path::Path;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use zeron_engine::arbos_capture::{self, CapturedWindow};

/// A scratch project: `.arbos` and nothing else, so the screenshots below are
/// only what this file adds.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".arbos")).unwrap();
    dir
}

/// One `screenshot_capture` call, minus the engine: the registered capture the
/// op reads, then the engine's own capture. The frame's other keys — `op`,
/// `project` — reach the dispatcher, not this function.
fn op_capture(
    project: &Path,
    now: chrono::DateTime<Utc>,
    capture: Option<arbos_capture::CaptureWindow>,
) -> Result<arbos_capture::ScreenshotView, String> {
    arbos_capture::capture_screenshot(project, now, capture)
}

/// A capture that always returns the same PNG, as the app's hook would.
fn capture_returning(png: Vec<u8>) -> arbos_capture::CaptureWindow {
    Arc::new(move || {
        Ok(CapturedWindow {
            png: png.clone(),
            window_title: Some("Arbos".into()),
        })
    })
}

/// A capture whose window cannot be taken, as the app's hook reports it.
fn capture_failing(message: &str) -> arbos_capture::CaptureWindow {
    let message = message.to_owned();
    Arc::new(move || Err(message.clone()))
}

fn picture(tag: u8) -> Vec<u8> {
    // A PNG signature plus a body the assertions can tell apart.
    let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    png.extend(std::iter::repeat(tag).take(16));
    png
}

fn written(project: &Path, relative: &str) -> Vec<u8> {
    std::fs::read(project.join(".arbos").join("context").join(relative)).unwrap()
}

fn screenshot_count(project: &Path) -> usize {
    std::fs::read_dir(project.join(".arbos").join("context").join("screenshots"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

#[test]
fn a_registered_capture_writes_a_png_under_the_context_and_returns_its_path() {
    let root = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 13, 14, 15, 30).unwrap();

    let view = op_capture(root.path(), now, Some(capture_returning(picture(7)))).unwrap();
    assert!(
        view.path
            .starts_with("screenshots/screenshot-20261013-141530-"),
        "{} does not name its directory and moment",
        view.path
    );
    assert!(view.path.ends_with(".png"), "{}", view.path);
    assert_eq!(view.bytes, picture(7).len());
    assert_eq!(view.window_title.as_deref(), Some("Arbos"));
    assert_eq!(written(root.path(), &view.path), picture(7));
    assert_eq!(screenshot_count(root.path()), 1);
}

#[test]
fn a_second_capture_in_the_same_second_gets_its_own_file() {
    let root = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 13, 14, 15, 30).unwrap();

    let first = op_capture(root.path(), now, Some(capture_returning(picture(1)))).unwrap();
    let second = op_capture(root.path(), now, Some(capture_returning(picture(2)))).unwrap();
    assert_ne!(first.path, second.path, "two captures never share a name");
    assert_eq!(written(root.path(), &first.path), picture(1));
    assert_eq!(written(root.path(), &second.path), picture(2));
    assert_eq!(screenshot_count(root.path()), 2);
}

#[test]
fn a_headless_engine_names_the_missing_window_in_plain_words() {
    let root = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 13, 14, 15, 30).unwrap();

    // No registered capture: a headless or remote engine owns no window.
    assert_eq!(
        op_capture(root.path(), now, None).unwrap_err(),
        "Arbos's project window is not available to capture here. Open the project in the Arbos app and try again."
    );
    assert_eq!(screenshot_count(root.path()), 0, "nothing was written");
}

#[test]
fn a_failed_capture_names_its_reason_and_writes_nothing() {
    let root = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 13, 14, 15, 30).unwrap();

    let reason = "No Arbos window is on screen to capture. Open the project window and try again.";
    assert_eq!(
        op_capture(root.path(), now, Some(capture_failing(reason))).unwrap_err(),
        reason
    );
    assert_eq!(
        screenshot_count(root.path()),
        0,
        "only a capture writes a file"
    );
}

#[test]
fn a_capture_that_is_not_a_png_is_refused_before_it_writes() {
    let root = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 13, 14, 15, 30).unwrap();

    assert_eq!(
        op_capture(root.path(), now, Some(capture_returning(vec![1, 2, 3]))).unwrap_err(),
        "the capture did not produce a PNG picture"
    );
    let oversized = {
        let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        png.resize(24 * 1024 * 1024 + 1, 0);
        png
    };
    assert_eq!(
        op_capture(root.path(), now, Some(capture_returning(oversized))).unwrap_err(),
        "the captured window is larger than Arbos's 24 MB picture limit."
    );
    assert_eq!(
        screenshot_count(root.path()),
        0,
        "no refused capture wrote a file"
    );
}
