//! Desktop self-update: the app's own release checker plus the download /
//! install lifecycle behind the sidebar update strip, "Check for Updates…"
//! (macOS app menu; the account menu elsewhere) and install-on-quit.
//!
//! The checker runs in this process against this binary's version. The
//! engine keeps its own checker for daemons and remote reporting, but the UI
//! may be attached to a daemon of another version, whose status describes
//! that binary rather than the app the user is looking at.
//!
//! Lifecycle, idiomatic to desktop updaters (Sparkle, VS Code, Zed): the
//! strip has one button, Update, that means "bring me current" — a press
//! starts the download if none is running, makes an already-running
//! background download install and relaunch the moment it is ready, and
//! installs and relaunches at once when the release is already staged.
//! The strip itself carries the download progress, so there is no second
//! press to remember. A staged update the user never pressed for still
//! installs when the app quits — nobody stays on a stale version just
//! because they never clicked. `ZERON_AUTO_UPDATE=0` keeps it report-only.

use std::path::{Path, PathBuf};

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Task};
use gpui_tokio::Tokio;
use zeron_update::{InstallKind, UpdateBlocker, UpdateStatus, Updater};

/// Download/install lifecycle of the newest release in this process.
#[derive(Debug, Clone, PartialEq)]
pub enum Flow {
    Idle,
    Downloading {
        version: String,
        /// Bytes moved so far, once the body started streaming.
        moved: Option<u64>,
        /// Total bytes, when the server announced them.
        total: Option<u64>,
        /// The download finishes into an install + relaunch instead of
        /// waiting for a quit (the user pressed Update while it ran).
        relaunch: bool,
    },
    /// Staged and verified: pressing Update installs it and relaunches now,
    /// or it is installed on quit.
    Ready {
        version: String,
        staged: PathBuf,
    },
    Failed {
        version: String,
        message: SharedString,
    },
    /// Swapped in; this process is on its way out.
    Installed,
}

/// The user-initiated check's dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    Checking,
    /// The check finished; the dialog renders from the live status + flow.
    Result,
    CheckFailed(SharedString),
}

/// What clicking the sidebar strip does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripAction {
    /// Nothing — the flow is finishing on its own.
    None,
    /// The single button: start, then take it all the way to a relaunch.
    /// What exactly a press does in this state is [`update_click`].
    Update,
    /// Explain why this install can't update itself (dialog).
    Explain,
    /// Advisory installs: open the releases page (unmanaged) and dismiss.
    Advise {
        open_releases: bool,
    },
}

/// What one press of the strip's Update button does, state by state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateClick {
    /// Nothing in flight: fetch, verify, and stage the release, then install
    /// it and relaunch.
    Start,
    /// A background download is already running: finish it, then install and
    /// relaunch at once (no second press needed).
    RelaunchWhenReady,
    /// The release is staged: install it and relaunch now.
    InstallNow,
    /// The last attempt failed: start over.
    Retry,
}

pub fn update_click(flow: &Flow) -> UpdateClick {
    match flow {
        Flow::Idle => UpdateClick::Start,
        // Pressing again while it already restarts-when-done is idempotent:
        // the promise is already made, the press just re-makes it.
        Flow::Downloading { .. } => UpdateClick::RelaunchWhenReady,
        Flow::Ready { .. } => UpdateClick::InstallNow,
        Flow::Failed { .. } => UpdateClick::Retry,
        // Nothing to update into; the process is exiting.
        Flow::Installed => UpdateClick::Retry,
    }
}

pub struct AppUpdate {
    install: InstallKind,
    blocker: Option<UpdateBlocker>,
    /// Background download + install on quit (`ZERON_AUTO_UPDATE` unset or on).
    automatic: bool,
    edge_url: String,
    data_dir: PathBuf,
    checker: Updater,
    status: Option<UpdateStatus>,
    flow: Flow,
    prompt: Option<Prompt>,
    /// Version whose advisory strip the user dismissed (a newer release shows
    /// it again).
    dismissed: Option<String>,
    /// The running download finishes into an install + relaunch instead of
    /// waiting for a quit. Set by a press of Update ([`AppUpdate::update_now`]);
    /// read when the download lands.
    relaunch_when_ready: bool,
    _status_watch: Task<()>,
    download: Option<Task<()>>,
    _progress_watch: Option<Task<()>>,
    user_check: Option<Task<()>>,
}

struct GlobalAppUpdate(Entity<AppUpdate>);

impl Global for GlobalAppUpdate {}

impl AppUpdate {
    /// Start the desktop checker. Call once at boot, after `gpui_tokio` is
    /// initialized.
    pub fn init(edge_url: String, data_dir: PathBuf, cx: &mut App) {
        let entity = cx.new(|cx| Self::new(edge_url, data_dir, cx));
        cx.set_global(GlobalAppUpdate(entity));
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAppUpdate>()
            .map(|global| global.0.clone())
    }

    fn new(edge_url: String, data_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let install = zeron_update::detect_install();
        let blocker = install
            .supports_desktop_update()
            .then(|| install.desktop_update_blocker())
            .flatten();
        let checker = {
            let _runtime = Tokio::handle(cx).enter();
            Updater::spawn_desktop(edge_url.clone())
        };
        let mut statuses = checker.watch();
        let status_watch = cx.spawn(async move |this, cx| {
            while statuses.changed().await.is_ok() {
                let status = statuses.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| this.apply_status(status, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        tracing::info!(?install, ?blocker, "desktop update checker started");
        Self {
            install,
            blocker,
            automatic: zeron_update::desktop_auto_update_enabled(),
            edge_url,
            data_dir,
            checker,
            status: None,
            flow: Flow::Idle,
            prompt: None,
            dismissed: None,
            relaunch_when_ready: false,
            _status_watch: status_watch,
            download: None,
            _progress_watch: None,
            user_check: None,
        }
    }

    pub fn install(&self) -> &InstallKind {
        &self.install
    }

    pub fn blocker(&self) -> Option<&UpdateBlocker> {
        self.blocker.as_ref()
    }

    pub fn flow(&self) -> &Flow {
        &self.flow
    }

    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    /// The newer release the last check found, if any.
    pub fn available(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|status| status.update_available)
            .and_then(|status| status.latest_version.as_deref())
    }

    /// Whether this app downloads and installs updates itself right now.
    pub fn self_updating(&self) -> bool {
        self.install.supports_desktop_update() && self.blocker.is_none()
    }

    /// Window activation / wake: check if one is due by the wall clock.
    pub fn poke(&self) {
        self.checker.poke();
    }

    fn apply_status(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        let succeeded = status.error.is_none();
        self.status = Some(status);
        if succeeded && self.automatic {
            self.download_if_needed(cx);
        }
        cx.notify();
    }

    /// Start (or restart) the background download unless the newest release
    /// is already downloading or staged. A failed download retries at the
    /// next successful check.
    fn download_if_needed(&mut self, cx: &mut Context<Self>) {
        if !self.self_updating() {
            return;
        }
        let Some(latest) = self.available() else {
            return;
        };
        let current = match &self.flow {
            Flow::Downloading { .. } | Flow::Installed => return,
            Flow::Ready { version, .. } => Some(version),
            Flow::Idle | Flow::Failed { .. } => None,
        };
        if current.is_some_and(|version| !zeron_update::version_newer(latest, version)) {
            return;
        }
        self.start_download(cx);
    }

    pub fn start_download(&mut self, cx: &mut Context<Self>) {
        self.start_download_with(false, cx);
    }

    /// Start (or restart) the download of the newest release. `relaunch` is
    /// the single button's promise: when the download lands, it installs and
    /// relaunches at once instead of waiting for a quit. The strip's live
    /// progress comes from a watch channel the staging path reports into.
    fn start_download_with(&mut self, relaunch: bool, cx: &mut Context<Self>) {
        if !self.self_updating() || matches!(self.flow, Flow::Downloading { .. }) {
            return;
        }
        let Some(version) = self.available().map(str::to_owned) else {
            return;
        };
        let edge_url = self.edge_url.clone();
        let data_dir = self.data_dir.clone();
        let install = self.install.clone();
        self.relaunch_when_ready = relaunch;
        self.flow = Flow::Downloading {
            version: version.clone(),
            moved: None,
            total: None,
            relaunch,
        };
        // (bytes moved, total when known) as the staging path reports it. A
        // watch rather than a queue: only the newest report renders.
        let (progress_tx, mut progress_rx) =
            tokio::sync::watch::channel((0u64, None::<u64>));
        let sink: zeron_update::ProgressFn = {
            let progress_tx = progress_tx.clone();
            std::sync::Arc::new(move |moved: u64, total: Option<u64>| {
                let _ = progress_tx.send((moved, total));
            })
        };
        let progress_watch = cx.spawn(async move |this, cx| {
            // Each report lands in the flow so the strip renders it live.
            while progress_rx.changed().await.is_ok() {
                let (moved, total) = *progress_rx.borrow_and_update();
                this.update(cx, |this, cx| {
                    if let Flow::Downloading {
                        moved: reported,
                        total: announced,
                        ..
                    } = &mut this.flow
                    {
                        *reported = Some(moved);
                        *announced = total;
                    }
                    cx.notify();
                })
                .ok();
            }
        });
        let stage = Tokio::spawn(cx, async move {
            // Re-read the manifest so a long-lived status still downloads the
            // newest release, with that release's checksums and signature.
            let manifest = zeron_update::fetch_latest(&edge_url).await?;
            anyhow::ensure!(
                zeron_update::version_newer(&manifest.version, zeron_update::current_version()),
                "the release feed no longer offers a newer version"
            );
            let staged = install
                .stage_desktop(&edge_url, &manifest, &data_dir, Some(sink))
                .await?;
            anyhow::Ok((manifest.version, staged))
        });
        self.download = Some(cx.spawn(async move |this, cx| {
            let outcome = match stage.await {
                Ok(Ok(staged)) => Ok(staged),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join) => Err(join.to_string()),
            };
            this.update(cx, |this, cx| {
                match outcome {
                    Ok((version, staged)) => {
                        tracing::info!(%version, staged = %staged.display(), "update staged");
                        this.flow = Flow::Ready { version, staged };
                        // The single button's promise: finish the job now.
                        // The quit goes through the app's own protocol so
                        // file buffers flush before the process goes away.
                        if this.relaunch_when_ready
                            && let Some(staged) = this.staged()
                            && this.install_for_restart(&staged, cx).is_ok()
                        {
                            crate::app_menus::request_quit(&mut *cx);
                        }
                    }
                    Err(message) => {
                        tracing::warn!(%message, "update download failed");
                        this.flow = Flow::Failed {
                            version,
                            message: message.into(),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        self._progress_watch = Some(progress_watch);
        cx.notify();
    }

    /// The strip's single Update button: one press carries the whole
    /// lifecycle. See [`update_click`] for what a press does in each state.
    pub fn update_now(&mut self, cx: &mut Context<Self>) {
        match update_click(&self.flow) {
            UpdateClick::Start | UpdateClick::Retry => {
                self.start_download_with(true, cx);
            }
            UpdateClick::RelaunchWhenReady => {
                self.relaunch_when_ready = true;
                if let Flow::Downloading { relaunch, .. } = &mut self.flow {
                    *relaunch = true;
                }
                cx.notify();
            }
            UpdateClick::InstallNow => {
                let Some(staged) = self.staged() else {
                    return;
                };
                if self.install_for_restart(&staged, cx).is_ok() {
                    // Buffer-flushing quit protocol; the relauncher arranged
                    // by the apply starts the new version once we exit.
                    crate::app_menus::request_quit(&mut *cx);
                }
            }
        }
    }

    /// "Check for Updates…": check now and report the outcome in a dialog. A
    /// found release starts downloading at once (it would anyway on the
    /// hourly check), and a failed download gets a fresh attempt.
    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.prompt = Some(Prompt::Checking);
        let checker = self.checker.clone();
        let check = Tokio::spawn(cx, async move { checker.check().await });
        self.user_check = Some(cx.spawn(async move |this, cx| {
            let result = match check.await {
                Ok(Ok(status)) => Ok(status),
                Ok(Err(err)) => {
                    tracing::warn!(error = %format!("{err:#}"), "update check failed");
                    // The dialog shows the cause; the log keeps the chain.
                    Err(err.root_cause().to_string())
                }
                Err(join) => Err(join.to_string()),
            };
            this.update(cx, |this, cx| {
                let outcome = match result {
                    Ok(status) => {
                        this.status = Some(status);
                        if this.automatic {
                            this.download_if_needed(cx);
                        }
                        Prompt::Result
                    }
                    Err(message) => Prompt::CheckFailed(message.into()),
                };
                // The user may have closed the dialog while it was checking.
                if this.prompt.is_some() {
                    this.prompt = Some(outcome);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn dismiss_prompt(&mut self, cx: &mut Context<Self>) {
        self.prompt = None;
        self.user_check = None;
        cx.notify();
    }

    /// Open the dialog on its current result (the strip's "explain" action).
    pub fn show_result(&mut self, cx: &mut Context<Self>) {
        self.prompt = Some(Prompt::Result);
        cx.notify();
    }

    /// The staged update the Update button installs.
    pub fn staged(&self) -> Option<PathBuf> {
        match &self.flow {
            Flow::Ready { staged, .. } => Some(staged.clone()),
            _ => None,
        }
    }

    /// Install `staged` and arrange the relaunch. The caller quits on success.
    pub fn install_for_restart(
        &mut self,
        staged: &Path,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let result = self.install.apply_desktop(staged, true);
        self.flow = match &result {
            Ok(()) => Flow::Installed,
            Err(err) => {
                tracing::error!(error = %err, "update apply failed");
                Flow::Failed {
                    version: self.available().unwrap_or_default().to_owned(),
                    message: format!("{err:#}").into(),
                }
            }
        };
        cx.notify();
        result
    }

    /// Quit hook: a staged update the user never restarted for installs now,
    /// so the next launch is current. Runs synchronously inside the quit.
    fn install_on_quit(&mut self) {
        if !self.automatic {
            return;
        }
        let Flow::Ready { version, staged } = &self.flow else {
            return;
        };
        match self.install.apply_desktop(staged, false) {
            Ok(()) => {
                tracing::info!(%version, "installed staged update on quit");
                self.flow = Flow::Installed;
            }
            Err(err) => tracing::warn!(error = %err, "installing the staged update on quit failed"),
        }
    }

    pub fn dismiss_advisory(&mut self, cx: &mut Context<Self>) {
        self.dismissed = self.available().map(str::to_owned);
        cx.notify();
    }

    /// The sidebar strip for the current state: `None` while there is
    /// nothing newer (or the advisory was dismissed for this version).
    pub fn strip(&self) -> Option<(SharedString, StripAction)> {
        let latest = self.available()?;
        if self.dismissed.as_deref() == Some(latest) {
            return None;
        }
        Some(strip_for(
            &self.install,
            self.blocker.is_some(),
            &self.flow,
            latest,
        ))
    }
}

/// Label + click action of the strip. Self-updating installs have one
/// button, Update, whose meaning per state is [`update_click`] — the strip
/// itself carries the download progress; blocked installs explain
/// themselves; managed installs without a desktop path get the
/// `arbos update` hint; unmanaged installs (source builds, hand-copied
/// binaries) are pointed at the GitHub releases page.
pub fn strip_for(
    install: &InstallKind,
    blocked: bool,
    flow: &Flow,
    latest: &str,
) -> (SharedString, StripAction) {
    if install.supports_desktop_update() {
        if blocked {
            return (
                format!("Update available — v{latest}").into(),
                StripAction::Explain,
            );
        }
        return match flow {
            Flow::Idle => (
                format!("Update available — v{latest}").into(),
                StripAction::Update,
            ),
            Flow::Downloading {
                version,
                moved,
                total,
                relaunch,
            } => {
                // The progress a person reads is in the strip itself; the
                // button stays live so one more press makes the finished
                // download restart the app.
                let mut label = format!("Downloading v{version}…");
                if let Some(moved) = moved {
                    label.push(' ');
                    label.push_str(&progress_text(*moved, *total));
                }
                if *relaunch {
                    label.push_str(" — restarts when done");
                }
                (label.into(), StripAction::Update)
            }
            Flow::Ready { version, .. } => (
                format!("Update to v{version} — restarts Arbos").into(),
                StripAction::Update,
            ),
            Flow::Failed { message, .. } => (
                format!("Update failed: {message}").into(),
                StripAction::Update,
            ),
            Flow::Installed => ("Restarting…".into(), StripAction::None),
        };
    }
    if matches!(install, InstallKind::Managed { .. }) {
        (
            format!("Update available — v{latest} · run `arbos update`").into(),
            StripAction::Advise {
                open_releases: false,
            },
        )
    } else {
        (
            format!("Update available — v{latest} · download from GitHub").into(),
            StripAction::Advise {
                open_releases: true,
            },
        )
    }
}

/// `12.3 MB of 91 MB` / `12.3 MB` — the download's live progress, one line.
fn progress_text(moved: u64, total: Option<u64>) -> String {
    match total {
        Some(total) => format!("{} of {}", human_bytes(moved), human_bytes(total)),
        None => human_bytes(moved),
    }
}

/// Bytes as a person reads them: `812 B`, `4 KB`, `34.2 MB`, `91 MB`, `1.4 GB`.
fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    // A whole number of megabytes reads best without the empty decimal.
    fn with_unit(value: f64, unit: &str) -> String {
        if value.fract() == 0.0 {
            format!("{} {unit}", value as u64)
        } else {
            format!("{value:.1} {unit}")
        }
    }
    if bytes >= GB {
        with_unit(bytes as f64 / GB as f64, "GB")
    } else if bytes >= MB {
        with_unit(bytes as f64 / MB as f64, "MB")
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

/// "Check for Updates…" from a menu: surface the main window, then check.
pub fn check_for_updates(cx: &mut App) {
    crate::activate_main_window(cx);
    if let Some(update) = AppUpdate::global(cx) {
        update.update(cx, |update, cx| update.check_for_updates(cx));
    }
}

/// See [`AppUpdate::install_on_quit`].
pub fn install_on_quit(cx: &mut App) {
    if let Some(update) = AppUpdate::global(cx) {
        update.update(cx, |update, _| update.install_on_quit());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_follows_the_flow_on_self_updating_installs() {
        let mac_app = InstallKind::MacApp {
            bundle: PathBuf::from("/Applications/Arbos.app"),
        };
        // One button throughout: Update starts the download…
        assert_eq!(
            strip_for(&mac_app, false, &Flow::Idle, "0.2.86"),
            (
                SharedString::from("Update available — v0.2.86"),
                StripAction::Update
            )
        );
        // …carries its progress in the strip itself…
        let downloading = Flow::Downloading {
            version: "0.2.86".into(),
            moved: None,
            total: None,
            relaunch: false,
        };
        assert_eq!(
            strip_for(&mac_app, false, &downloading, "0.2.86"),
            (
                SharedString::from("Downloading v0.2.86…"),
                StripAction::Update
            )
        );
        let mid_download = Flow::Downloading {
            version: "0.2.86".into(),
            moved: Some(34 * 1024 * 1024),
            total: Some(91 * 1024 * 1024),
            relaunch: false,
        };
        assert_eq!(
            strip_for(&mac_app, false, &mid_download, "0.2.86"),
            (
                SharedString::from("Downloading v0.2.86… 34 MB of 91 MB"),
                StripAction::Update
            )
        );
        // …and a press during it promises the restart, in the label.
        let relaunching = Flow::Downloading {
            version: "0.2.86".into(),
            moved: Some(34 * 1024 * 1024),
            total: Some(91 * 1024 * 1024),
            relaunch: true,
        };
        assert_eq!(
            strip_for(&mac_app, false, &relaunching, "0.2.86"),
            (
                SharedString::from("Downloading v0.2.86… 34 MB of 91 MB — restarts when done"),
                StripAction::Update
            )
        );
        // Staged: the same press installs and relaunches now.
        let ready = Flow::Ready {
            version: "0.2.86".into(),
            staged: PathBuf::from("/tmp/Arbos.app"),
        };
        assert_eq!(
            strip_for(&mac_app, false, &ready, "0.2.86"),
            (
                SharedString::from("Update to v0.2.86 — restarts Arbos"),
                StripAction::Update
            )
        );
        // A failure keeps the button: the press retries.
        let failed = Flow::Failed {
            version: "0.2.86".into(),
            message: "offline".into(),
        };
        assert_eq!(
            strip_for(&mac_app, false, &failed, "0.2.86"),
            (
                SharedString::from("Update failed: offline"),
                StripAction::Update
            )
        );
        // A translocated or read-only bundle explains instead of failing.
        assert_eq!(
            strip_for(&mac_app, true, &Flow::Idle, "0.2.86").1,
            StripAction::Explain
        );
    }

    #[test]
    fn one_press_means_the_whole_lifecycle_in_every_state() {
        assert_eq!(update_click(&Flow::Idle), UpdateClick::Start);
        assert_eq!(
            update_click(&Flow::Downloading {
                version: "0.2.86".into(),
                moved: None,
                total: None,
                relaunch: false,
            }),
            UpdateClick::RelaunchWhenReady
        );
        // Pressing again while it already restarts-when-done is idempotent.
        assert_eq!(
            update_click(&Flow::Downloading {
                version: "0.2.86".into(),
                moved: Some(1),
                total: Some(2),
                relaunch: true,
            }),
            UpdateClick::RelaunchWhenReady
        );
        assert_eq!(
            update_click(&Flow::Ready {
                version: "0.2.86".into(),
                staged: PathBuf::from("/tmp/Arbos.app"),
            }),
            UpdateClick::InstallNow
        );
        assert_eq!(
            update_click(&Flow::Failed {
                version: "0.2.86".into(),
                message: "offline".into(),
            }),
            UpdateClick::Retry
        );
    }

    #[test]
    fn progress_reads_as_a_person_expects() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(812), "812 B");
        assert_eq!(human_bytes(4 * 1024), "4 KB");
        // A whole megabyte drops the empty decimal; a real fraction keeps one.
        assert_eq!(human_bytes(34 * 1024 * 1024), "34 MB");
        assert_eq!(human_bytes(91 * 1024 * 1024), "91 MB");
        assert_eq!(human_bytes(36071014), "34.4 MB");
        assert_eq!(human_bytes((1.4 * 1024.0 * 1024.0 * 1024.0) as u64), "1.4 GB");
        assert_eq!(
            progress_text(34 * 1024 * 1024, Some(91 * 1024 * 1024)),
            "34 MB of 91 MB"
        );
        // A stream nobody sized still shows how much has arrived.
        assert_eq!(progress_text(34 * 1024 * 1024, None), "34 MB");
    }

    #[test]
    fn strip_advises_installs_without_a_desktop_path() {
        let unmanaged = strip_for(&InstallKind::Unmanaged, false, &Flow::Idle, "0.2.86");
        assert_eq!(
            unmanaged,
            (
                SharedString::from("Update available — v0.2.86 · download from GitHub"),
                StripAction::Advise {
                    open_releases: true
                }
            )
        );
        let managed = InstallKind::Managed {
            app_root: PathBuf::from("/home/u/.zeron/app"),
        };
        let strip = strip_for(&managed, false, &Flow::Idle, "0.2.86");
        if cfg!(target_os = "linux") {
            // Linux desktop installs share the managed layout and update
            // themselves like the other desktop platforms.
            assert_eq!(strip.1, StripAction::Update);
            let ready = Flow::Ready {
                version: "0.2.86".into(),
                staged: PathBuf::from("/tmp/staged"),
            };
            assert_eq!(
                strip_for(&managed, false, &ready, "0.2.86"),
                (
                    SharedString::from("Update to v0.2.86 — restarts Arbos"),
                    StripAction::Update
                )
            );
        } else {
            assert_eq!(
                strip.0,
                SharedString::from("Update available — v0.2.86 · run `arbos update`")
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_installs_drive_the_desktop_flow() {
        let installed = InstallKind::WindowsPortable {
            directory: PathBuf::from(r"C:\Users\u\AppData\Local\Programs\Zeron"),
        };
        assert_eq!(
            strip_for(&installed, false, &Flow::Idle, "0.2.86"),
            (
                SharedString::from("Update available — v0.2.86"),
                StripAction::Update
            )
        );
    }
}
