//! Provider and tool keys for the fx child.
//!
//! macOS stores each key in the login keychain under the service `arbos`.
//! Linux stores each key in a mode 600 file outside the project. The value is
//! passed to `fx acp` as an environment variable. It is not written to a
//! project file, a settings file, or a log line.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::Duration;

/// Secret name and the environment variable fx reads.
pub const PROVIDERS: &[(&str, &str)] = &[
    ("engy", "ENGY_API_KEY"),
    ("ai-gateway", "AI_GATEWAY_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
    ("codex", "CODEX_API_KEY"),
    ("grok", "GROK_API_KEY"),
];

/// Secret name and the environment variable a worker's tools read. `gh`
/// reads `GH_TOKEN` itself; `op` reads the service account token.
///
/// The rule for a tool token: it reaches a worker through the fx child's
/// environment only. It is never written under a project, never logged, and
/// never returned by an op or offered to a model as a tool argument.
pub const TOOLS: &[(&str, &str)] = &[
    ("github", "GH_TOKEN"),
    ("onepassword", "OP_SERVICE_ACCOUNT_TOKEN"),
];

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "arbos";

/// The variable for a provider or tool secret name.
fn secret_env(name: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .chain(TOOLS)
        .find(|(secret, _)| *secret == name)
        .map(|(_, env)| *env)
}

/// Directory for the Linux files. `ARBOS_SECRETS_DIR` overrides the default
/// `~/.local/share/arbos/secrets`. A path inside `.arbos` is rejected.
pub fn secrets_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("ARBOS_SECRETS_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            let path = PathBuf::from(dir);
            reject_project_path(&path)?;
            return Ok(path);
        }
    }
    let home = crate::executable::home_dir().ok_or_else(|| "home directory is missing".to_string())?;
    Ok(home.join(".local").join("share").join("arbos").join("secrets"))
}

fn reject_project_path(path: &Path) -> Result<(), String> {
    if path
        .components()
        .any(|component| component.as_os_str() == ".arbos")
    {
        return Err("a key cannot be stored inside .arbos".into());
    }
    if !path.is_absolute() {
        return Err("the secrets directory must be an absolute path".into());
    }
    Ok(())
}

/// Copy a key that is already in the process environment into the store when
/// the store does not have it yet, then fill any empty provider or tool
/// variable from the store. The value is never logged.
pub fn install_process_env() {
    if let Ok(value) = std::env::var("ENGY_API_KEY") {
        let value = value.trim();
        if !value.is_empty() && read("engy").is_none() {
            let _ = write("engy", value);
        }
    }
    for (name, var_name) in PROVIDERS.iter().chain(TOOLS) {
        if env_present(var_name) {
            continue;
        }
        if let Some(value) = read(name) {
            // The fx child inherits this process environment. The value is not logged.
            unsafe { std::env::set_var(var_name, value) };
        }
    }
}

fn env_present(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

/// Stored provider variables other than `ENGY_API_KEY`, which the spawn path
/// already passes. A variable already set on this process is left to be inherited.
pub fn provider_env_pairs() -> Vec<(&'static str, String)> {
    PROVIDERS
        .iter()
        .filter(|(name, var_name)| *name != "engy" && !env_present(var_name))
        .filter_map(|(name, var_name)| read(name).map(|value| (*var_name, value)))
        .collect()
}

/// Stored tool variables for the fx child. A variable already set on this
/// process is left to be inherited.
pub fn tool_env_pairs() -> Vec<(&'static str, String)> {
    TOOLS
        .iter()
        .filter(|(_, var_name)| !env_present(var_name))
        .filter_map(|(name, var_name)| read(name).map(|value| (*var_name, value)))
        .collect()
}

pub fn read(name: &str) -> Option<String> {
    if secret_env(name).is_none() {
        return None;
    }
    let value = read_raw(name)?;
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

pub fn write(name: &str, value: &str) -> Result<(), String> {
    if secret_env(name).is_none() {
        return Err("unknown provider".into());
    }
    let value = value.trim();
    if value.is_empty() {
        return Err("the key is empty".into());
    }
    if value.contains('\0') || value.contains('\n') {
        return Err("the key has a newline".into());
    }
    write_raw(name, value)
}

/// `keychain` on macOS, `file` on Linux.
pub fn place() -> &'static str {
    if cfg!(target_os = "macos") {
        "keychain"
    } else {
        "file"
    }
}

#[cfg(target_os = "macos")]
fn read_raw(name: &str) -> Option<String> {
    let (ok, stdout, _) = security(&[
        "find-generic-password",
        "-s",
        KEYCHAIN_SERVICE,
        "-a",
        name,
        "-w",
    ]);
    ok.then(|| stdout.trim_end_matches('\n').to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "macos")]
fn write_raw(name: &str, value: &str) -> Result<(), String> {
    let (ok, _, stderr) = security(&[
        "add-generic-password",
        "-U",
        "-s",
        KEYCHAIN_SERVICE,
        "-a",
        name,
        "-w",
        value,
    ]);
    if ok {
        Ok(())
    } else {
        let detail = stderr.trim();
        Err(if detail.is_empty() {
            "keychain write failed".into()
        } else {
            format!("keychain write failed: {detail}")
        })
    }
}

#[cfg(target_os = "macos")]
fn security(args: &[&str]) -> (bool, String, String) {
    let run = Command::new("/usr/bin/security")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(run);
    });
    match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(out)) => (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ),
        _ => (false, String::new(), "security timed out".into()),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_raw(name: &str) -> Option<String> {
    let path = secrets_dir().ok()?.join(name);
    let text = std::fs::read_to_string(path).ok()?;
    Some(text)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_raw(name: &str, value: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let dir = secrets_dir()?;
    reject_project_path(&dir)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|err| err.to_string())?;
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| err.to_string())?;
    file.write_all(value.as_bytes())
        .map_err(|err| err.to_string())?;
    file.flush().map_err(|err| err.to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| err.to_string())?;
    let mode = std::fs::metadata(&path)
        .map_err(|err| err.to_string())?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o600 {
        return Err("the key file is not mode 600".into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn read_raw(_name: &str) -> Option<String> {
    None
}

#[cfg(not(unix))]
fn write_raw(_name: &str, _value: &str) -> Result<(), String> {
    Err("key storage is not available on this system".into())
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// Tests share one process and these tests set the same variables, so
    /// they take turns. No other test in this crate reads the variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn point_store_at(dir: &Path) {
        unsafe {
            std::env::set_var("ARBOS_SECRETS_DIR", dir);
            std::env::remove_var("GH_TOKEN");
            std::env::remove_var("OP_SERVICE_ACCOUNT_TOKEN");
            std::env::remove_var("GIT_CONFIG_COUNT");
        }
    }

    /// Every regular file under `root`, recursively, whose bytes contain `needle`.
    fn files_containing(root: &Path, needle: &[u8]) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if std::fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
                {
                    found.push(path);
                }
            }
        }
        found.sort();
        found
    }

    /// A `tracing` writer that keeps every line in memory.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    #[test]
    fn a_stored_github_token_reaches_the_tool_env_and_an_unknown_name_is_refused() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        point_store_at(dir.path());

        write("github", "test-token-not-real").unwrap();
        assert_eq!(read("github").as_deref(), Some("test-token-not-real"));
        let pairs = tool_env_pairs();
        assert_eq!(pairs, vec![("GH_TOKEN", "test-token-not-real".to_owned())]);
        assert!(
            provider_env_pairs()
                .iter()
                .all(|(name, _)| *name != "GH_TOKEN")
        );

        assert_eq!(
            write("nonsense", "test-token-not-real"),
            Err("unknown provider".to_owned())
        );
        assert!(read("nonsense").is_none());
        assert!(!dir.path().join("nonsense").exists());
    }

    #[test]
    fn a_github_token_reaches_the_fx_child_env_once_and_lands_nowhere_else() {
        const TOKEN: &str = "test-token-github-0123456789abcdef";
        let _guard = env_lock();
        let store = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let fx_home = project.path().join(".arbos").join("fx-home");
        let fx_home_text = fx_home.display().to_string();
        point_store_at(store.path());

        // Everything that touches the store runs under a subscriber that keeps
        // every line, at every level, so a logged value would show up here.
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let pairs = tracing::subscriber::with_default(subscriber, || {
            tracing::info!("the secrets test is logging");
            write("github", TOKEN).unwrap();
            assert_eq!(read("github").as_deref(), Some(TOKEN));

            // The store fills the process variable for the next fx child.
            install_process_env();
            assert_eq!(std::env::var("GH_TOKEN").as_deref(), Ok(TOKEN));
            // Cleared again so the child gets it from the store, as a fresh
            // engine does, and the git helper still follows from it.
            unsafe { std::env::remove_var("GH_TOKEN") };
            assert_eq!(tool_env_pairs(), vec![("GH_TOKEN", TOKEN.to_owned())]);

            crate::acp::fx_child_env_pairs(Some("test-key-not-real"), &fx_home_text)
        });

        // (a) The child environment: the token once, under GH_TOKEN, and the
        // git credential helper that reads it from there.
        let carrying: Vec<&str> = pairs
            .iter()
            .filter(|(name, value)| name.contains(TOKEN) || value.contains(TOKEN))
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(carrying, vec!["GH_TOKEN"], "{pairs:?}");
        assert_eq!(
            pairs.iter().filter(|(name, _)| name == "GH_TOKEN").count(),
            1
        );
        let helpers: Vec<&str> = pairs
            .iter()
            .filter(|(name, _)| name.starts_with("GIT_CONFIG_VALUE_"))
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(helpers, vec!["!gh auth git-credential"]);
        assert!(pairs.iter().any(|(name, value)| name == "GIT_CONFIG_KEY_0"
            && value == "credential.https://github.com.helper"));

        // (b) The store: one mode 600 file holds the token, outside the project.
        let github = store.path().join("github");
        assert_eq!(
            files_containing(store.path(), TOKEN.as_bytes()),
            vec![github.clone()]
        );
        let mode = std::fs::metadata(&github).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!store.path().starts_with(project.path()));
        assert!(!github.starts_with(project.path()));
        assert!(!fx_home.exists());
        assert!(files_containing(project.path(), TOKEN.as_bytes()).is_empty());

        // (c) The log: lines were captured, and none of them carries the token.
        let log = capture.text();
        assert!(log.contains("the secrets test is logging"), "{log}");
        assert!(!log.contains(TOKEN), "{log}");
        assert!(!log.contains("test-key-not-real"), "{log}");

        unsafe { std::env::remove_var("GH_TOKEN") };
    }
}
