//! `arbos-updatectl` — what "Release main" signs and publishes an update with,
//! and what a person sets the signing key up with once.
//!
//!   arbos-updatectl sign --manifest dist/manifest.json --version 0.2.112.1877 \
//!       [--commit 9f552c5] [--notes-file dist/notes.txt] \
//!       --artifact dist/arbos-0.2.112.1877-macos-arm64-app.tar.gz \
//!       --artifact dist/arbos-0.2.112.1877-linux-x86_64.tar.gz
//!       Every artifact digested and signed into `manifest.json`, the document
//!       the app reads, and the version alone into `latest.txt` beside it.
//!
//!   arbos-updatectl verify --manifest dist/manifest.json --dir dist
//!       Whether every file the manifest names is in `--dir`, has the digest
//!       it claims, and is signed by the key compiled into this build. Exits
//!       non-zero when any is not — which is how the publish job proves the
//!       secret and `update-key.pub` are the two halves of one key.
//!
//!   arbos-updatectl keygen
//!       A fresh key pair. The public line goes to stderr, to be pasted into
//!       `update-key.pub` and committed; the secret half goes to stdout and
//!       nowhere else, so it can be piped straight into
//!       `gh secret set ARBOS_UPDATE_SIGNING_KEY` without landing in a file
//!       or a shell history.
//!
//! The secret is read from `ARBOS_UPDATE_SIGNING_KEY` and never from an
//! argument: an argument is visible to every other process on the machine.

use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::ExitCode,
};
use zeron_update::{
    FileMeta, Manifest,
    sign::{self, PublicKey, SecretKey},
};

/// The variable CI puts the private half in.
const KEY_ENV: &str = "ARBOS_UPDATE_SIGNING_KEY";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("arbos-updatectl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut argv = std::env::args().skip(1);
    let command = argv.next().unwrap_or_default();
    let args = Args::parse(argv)?;
    match command.as_str() {
        "sign" => sign_manifest(&args),
        "verify" => verify_manifest(&args),
        "keygen" => keygen(&args),
        "" | "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => bail!("no such command `{other}`\n\n{USAGE}"),
    }
}

const USAGE: &str = "\
arbos-updatectl sign    --manifest <path> --version <x.y.z.n> [--commit <sha>]
                        [--notes-file <path>] --artifact <file> ...
arbos-updatectl verify  --manifest <path> --dir <directory>
arbos-updatectl keygen

The signing key is read from ARBOS_UPDATE_SIGNING_KEY.";

fn sign_manifest(args: &Args) -> Result<()> {
    args.no_free()?;
    let manifest_path = PathBuf::from(args.need("manifest")?);
    let version = args.need("version")?.trim().to_owned();
    if !version_is_dotted_numbers(&version) {
        bail!("--version `{version}` is not dotted numbers; the app could not order it");
    }
    let commit = args.one("commit").map(|c| c.trim().to_owned());
    let notes = match args.one("notes-file") {
        Some(path) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading {path}"))?
                .trim()
                .to_owned(),
        ),
        None => None,
    };
    let artifacts = args.all("artifact");
    if artifacts.is_empty() {
        bail!("no --artifact: a release with nothing to download is not a release");
    }
    let secret = secret_key()?;

    let mut files = BTreeMap::new();
    for artifact in &artifacts {
        let path = Path::new(artifact);
        let name = file_name(path)?;
        let bytes = read(path)?;
        let meta = FileMeta {
            sha256: Some(sign::sha256_hex(&bytes)),
            signature: Some(secret.sign(&bytes)?),
        };
        eprintln!(
            "{name}: {} bytes, sha256 {}",
            bytes.len(),
            meta.sha256.as_deref().unwrap_or_default()
        );
        if files.insert(name.clone(), meta).is_some() {
            bail!("{name} was given twice");
        }
    }

    let manifest = Manifest {
        version: version.clone(),
        commit,
        notes: notes.filter(|n| !n.is_empty()),
        files,
    };
    let mut json = serde_json::to_string_pretty(&manifest).context("writing manifest")?;
    json.push('\n');
    write(&manifest_path, json.as_bytes())?;
    let latest_path = manifest_path.with_file_name("latest.txt");
    write(&latest_path, format!("{version}\n").as_bytes())?;
    eprintln!(
        "{}: {version}, {} file(s) signed; {}",
        manifest_path.display(),
        manifest.files.len(),
        latest_path.display()
    );
    eprintln!(
        "signed with the key whose public half is `{}`",
        secret.public()?.to_line()
    );
    Ok(())
}

fn verify_manifest(args: &Args) -> Result<()> {
    args.no_free()?;
    let manifest_path = PathBuf::from(args.need("manifest")?);
    let dir = PathBuf::from(args.need("dir")?);
    let key = sign::built_in_key()
        .context("this build has no update key compiled in (update-key.pub has no key line)")?;
    let manifest: Manifest = serde_json::from_slice(&read(&manifest_path)?)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    if !version_is_dotted_numbers(&manifest.version) {
        bail!(
            "{} names version `{}`, which is not dotted numbers",
            manifest_path.display(),
            manifest.version
        );
    }
    if manifest.files.is_empty() {
        bail!("{} signs no files", manifest_path.display());
    }
    for (name, meta) in &manifest.files {
        verify_one(&key, &dir, name, meta).with_context(|| format!("{name} does not verify"))?;
        println!("ok {name}");
    }
    println!(
        "{}: {} file(s) verified against {}",
        manifest_path.display(),
        manifest.files.len(),
        key.to_line()
    );
    Ok(())
}

/// One manifest entry against the file beside it: present, the digest it
/// claims, and signed by `key`.
fn verify_one(key: &PublicKey, dir: &Path, name: &str, meta: &FileMeta) -> Result<()> {
    if name.contains('/') || name.contains('\\') || name == ".." {
        bail!("`{name}` is not a bare file name");
    }
    let bytes = read(&dir.join(name))?;
    if let Some(expected) = &meta.sha256 {
        let actual = sign::sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            bail!("sha256 is {actual}, manifest says {expected}");
        }
    }
    let signature = meta
        .signature
        .as_deref()
        .context("the manifest carries no signature for it")?;
    key.verify(&bytes, signature)
}

fn keygen(args: &Args) -> Result<()> {
    args.no_free()?;
    let (secret, public) = sign::generate()?;
    eprintln!("public half — put this line in app/crates/update/update-key.pub and commit it:");
    eprintln!("  {}", public.to_line());
    eprintln!("the secret half is on stdout; pipe it into `gh secret set {KEY_ENV}`");
    println!("{}", secret.to_base64());
    Ok(())
}

/// What the app's own version compare accepts: `0.2.112.1877`.
fn version_is_dotted_numbers(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn secret_key() -> Result<SecretKey> {
    let text = std::env::var(KEY_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .with_context(|| {
            format!("{KEY_ENV} is not set — run `arbos-updatectl keygen` once and store it")
        })?;
    SecretKey::from_base64(&text).with_context(|| KEY_ENV.to_owned())
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no file name", path.display()))?
        .to_owned())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// `--name value`, repeatable, plus whatever is left over.
///
/// Hand-rolled because this tool has three commands and no ambiguity in any
/// of them, and the app's dependency graph should not grow an argument parser
/// for a CI helper.
struct Args {
    flags: BTreeMap<String, Vec<String>>,
    free: Vec<String>,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self> {
        let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut free = Vec::new();
        let mut argv = argv.peekable();
        while let Some(arg) = argv.next() {
            match arg.strip_prefix("--") {
                Some(name) => {
                    let (name, value) = match name.split_once('=') {
                        Some((name, value)) => (name.to_owned(), value.to_owned()),
                        None => (
                            name.to_owned(),
                            argv.next()
                                .with_context(|| format!("--{name} wants a value"))?,
                        ),
                    };
                    flags.entry(name).or_default().push(value);
                }
                None => free.push(arg),
            }
        }
        Ok(Self { flags, free })
    }

    fn one(&self, name: &str) -> Option<String> {
        self.flags.get(name)?.last().cloned()
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }

    fn need(&self, name: &str) -> Result<String> {
        self.one(name)
            .with_context(|| format!("--{name} is needed"))
    }

    fn no_free(&self) -> Result<()> {
        match self.free.as_slice() {
            [] => Ok(()),
            stray => bail!("unexpected argument(s): {}\n\n{USAGE}", stray.join(" ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Args, FileMeta, Manifest, sign, verify_one, version_is_dotted_numbers};

    #[test]
    fn reads_repeated_flags_and_both_spellings() {
        let args = Args::parse(
            [
                "--manifest",
                "m.json",
                "--artifact=a.tar.gz",
                "--artifact",
                "b.tar.gz",
                "left-over",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(args.one("manifest").as_deref(), Some("m.json"));
        assert_eq!(args.all("artifact"), ["a.tar.gz", "b.tar.gz"]);
        assert_eq!(args.free, ["left-over"]);
        assert!(args.need("missing").is_err());
        assert!(args.no_free().is_err());
    }

    #[test]
    fn a_flag_with_no_value_is_an_error() {
        assert!(Args::parse(["--manifest"].into_iter().map(String::from)).is_err());
    }

    #[test]
    fn versions_are_dotted_numbers() {
        assert!(version_is_dotted_numbers("0.2.112.1877"));
        assert!(version_is_dotted_numbers("0.2.112"));
        for bad in ["", "v0.2.112", "0.2.", ".0.2", "0.2.112+1877", "nightly"] {
            assert!(!version_is_dotted_numbers(bad), "accepted `{bad}`");
        }
    }

    #[test]
    fn a_signed_manifest_verifies_and_a_tampered_byte_does_not() {
        let (secret, public) = sign::generate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let name = "arbos-0.2.112.1877-linux-x86_64.tar.gz";
        let payload = b"the tarball's bytes".to_vec();
        std::fs::write(dir.path().join(name), &payload).unwrap();
        let meta = FileMeta {
            sha256: Some(sign::sha256_hex(&payload)),
            signature: Some(secret.sign(&payload).unwrap()),
        };
        // The manifest round-trips through JSON the way the publish job
        // writes and the app reads it.
        let manifest = Manifest {
            version: "0.2.112.1877".into(),
            commit: Some("9f552c5".into()),
            notes: Some("Fix the thing".into()),
            files: [(name.to_owned(), meta)].into(),
        };
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, manifest);
        verify_one(&public, dir.path(), name, &back.files[name]).unwrap();

        // One byte changed on disk: the digest catches it first, and with the
        // digest gone the signature catches it alone.
        let mut tampered = payload.clone();
        tampered[0] ^= 1;
        std::fs::write(dir.path().join(name), &tampered).unwrap();
        let error = verify_one(&public, dir.path(), name, &back.files[name]).unwrap_err();
        assert!(error.to_string().contains("sha256"), "{error:#}");
        let no_digest = FileMeta {
            sha256: None,
            ..back.files[name].clone()
        };
        let error = verify_one(&public, dir.path(), name, &no_digest).unwrap_err();
        assert!(error.to_string().contains("signature"), "{error:#}");

        // Back to the real bytes, but signed by another key, or not at all.
        std::fs::write(dir.path().join(name), &payload).unwrap();
        let (other, _) = sign::generate().unwrap();
        let foreign = FileMeta {
            sha256: Some(sign::sha256_hex(&payload)),
            signature: Some(other.sign(&payload).unwrap()),
        };
        assert!(verify_one(&public, dir.path(), name, &foreign).is_err());
        let unsigned = FileMeta {
            sha256: Some(sign::sha256_hex(&payload)),
            signature: None,
        };
        assert!(verify_one(&public, dir.path(), name, &unsigned).is_err());
        // A file the manifest names but the directory lacks.
        assert!(verify_one(&public, dir.path(), "missing.tar.gz", &back.files[name]).is_err());
        // A name that would walk out of the directory.
        assert!(verify_one(&public, dir.path(), "../escape.tar.gz", &back.files[name]).is_err());
    }
}
