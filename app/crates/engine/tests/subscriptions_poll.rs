//! `github_pr` polls GitHub through the person's stored key and wakes Main.
//!
//! Like `github_cli_login_shell.rs`, this file must stay a single test: it
//! mutates the process PATH so `gh` resolves to a script this test writes,
//! and turns the login-shell snapshot off, so it needs its own test binary
//! with no parallel siblings.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::{Value, json};
use zeron_engine::arbos_wakes::{self, Firing};

/// The key Settings Keys would hold. The stub refuses to answer without it.
const TOKEN: &str = "test-token-github-not-real";
const SHA: &str = "ce22ddd5617830793e8836b4374f70008b3e8343";

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The stub `gh`: it answers only with the stored key, never prompts, and
/// prints whatever pull request JSON the test has staged beside it.
fn stub_gh(dir: &Path) {
    write_executable(
        &dir.join("gh"),
        &format!(
            "#!/bin/sh\n\
             if [ \"$GH_PROMPT_DISABLED\" != \"1\" ]; then\n\
             \techo \"interactive auth was not disabled\" >&2\n\
             \texit 2\n\
             fi\n\
             if [ \"$GH_TOKEN\" != \"{TOKEN}\" ]; then\n\
             \techo \"the stored key was not passed\" >&2\n\
             \texit 3\n\
             fi\n\
             cat \"{}\"\n",
            dir.join("pr.json").display(),
        ),
    );
}

fn stage_pr(dir: &Path, value: &Value) {
    std::fs::write(dir.join("pr.json"), serde_json::to_string(value).unwrap()).unwrap();
}

fn pr_view(state: &str, checks: Value) -> Value {
    json!({
        "state": state,
        "headRefOid": SHA,
        "title": "Show waiting for a worker whose model settles silently",
        "url": "https://github.com/unarbos/arbos/pull/849",
        "statusCheckRollup": checks,
    })
}

#[test]
fn a_github_pr_subscription_polls_with_the_stored_key_and_wakes_main() {
    let dir = tempfile::tempdir().unwrap();
    stub_gh(dir.path());

    // A GUI/service launch: the daemon's own PATH would miss `gh`, and the
    // login-shell snapshot stays off so the stub is the only `gh` there is.
    // SAFETY: single-test binary — nothing else reads env concurrently.
    unsafe {
        std::env::set_var("ZERON_NO_LOGIN_SHELL", "1");
        let composed = std::env::join_paths(
            std::iter::once(dir.path().to_path_buf()).chain(
                std::env::var_os("PATH")
                    .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
                    .unwrap_or_default(),
            ),
        )
        .unwrap();
        std::env::set_var("PATH", composed);
    }

    let mut key = String::new();

    // CI still running: no wake yet.
    stage_pr(
        dir.path(),
        &pr_view(
            "OPEN",
            json!([
                {"name": "CI", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"name": "test", "status": "IN_PROGRESS", "conclusion": null},
            ]),
        ),
    );
    assert_eq!(
        arbos_wakes::poll_github_pr("unarbos/arbos", 849, Some(TOKEN), &mut key).unwrap(),
        None
    );

    // CI finishes green: one wake, once per commit.
    stage_pr(
        dir.path(),
        &pr_view(
            "OPEN",
            json!([
                {"name": "CI", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"name": "test", "status": "COMPLETED", "conclusion": "SUCCESS"},
            ]),
        ),
    );
    let fields = arbos_wakes::poll_github_pr("unarbos/arbos", 849, Some(TOKEN), &mut key)
        .unwrap()
        .unwrap();
    assert_eq!(fields["pr"], 849);
    assert_eq!(fields["repo"], "unarbos/arbos");
    assert_eq!(fields["sha"], SHA);
    assert_eq!(fields["passed"], 2);
    assert_eq!(fields["failed"], 0);
    assert_eq!(
        arbos_wakes::poll_github_pr("unarbos/arbos", 849, Some(TOKEN), &mut key).unwrap(),
        None,
        "the finished commit wakes once"
    );

    // The pull request merges: the second wake.
    stage_pr(dir.path(), &pr_view("MERGED", json!([])));
    let fields = arbos_wakes::poll_github_pr("unarbos/arbos", 849, Some(TOKEN), &mut key)
        .unwrap()
        .unwrap();
    assert_eq!(fields["state"], "merged");
    assert_eq!(
        arbos_wakes::poll_github_pr("unarbos/arbos", 849, Some(TOKEN), &mut key).unwrap(),
        None,
        "the merge wakes once"
    );

    // The wake Main receives names the pull request and ends with the note.
    let firing = Firing {
        kind: "github_pr".into(),
        subscription: "pr-849".into(),
        at: "2026-10-13T03:03:00Z".into(),
        summary: "Pull request 849 on unarbos/arbos merged.".into(),
        fields,
        prompt: "Say what changed in the pull request.".into(),
    };
    let body = arbos_wakes::wake_body(&[firing]);
    assert!(body.starts_with("Pull request 849 on unarbos/arbos merged.\n<system_notification>"));
    assert!(body.contains("\"pr\":849"));
    assert!(body.contains("\"state\":\"merged\""));
    assert!(body.contains("Say what changed in the pull request.\n"));
    assert!(body.ends_with(arbos_wakes::UNTRUSTED_NOTE));

    // Without the key the subscription skips and reports; the stub is never
    // asked, because it refuses any call without the key, and nothing fails.
    let mut bare = String::new();
    assert_eq!(
        arbos_wakes::poll_github_pr("unarbos/arbos", 849, None, &mut bare).unwrap(),
        None
    );
    assert_eq!(bare, "");
}
