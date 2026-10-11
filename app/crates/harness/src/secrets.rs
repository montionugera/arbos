//! Provider keys for the fx child.
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

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "arbos";

fn provider_env(name: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|(provider, _)| *provider == name)
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
/// the store does not have it yet, then fill any empty provider variable from
/// the store. The value is never logged.
pub fn install_process_env() {
    if let Ok(value) = std::env::var("ENGY_API_KEY") {
        let value = value.trim();
        if !value.is_empty() && read("engy").is_none() {
            let _ = write("engy", value);
        }
    }
    for (name, var_name) in PROVIDERS {
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

pub fn read(name: &str) -> Option<String> {
    if provider_env(name).is_none() {
        return None;
    }
    let value = read_raw(name)?;
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

pub fn write(name: &str, value: &str) -> Result<(), String> {
    if provider_env(name).is_none() {
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
