//! `arbos-updatectl` — what CI signs and publishes an update with, and what a
//! person sets the signing key up with once.
//!
//!   arbos-updatectl keygen --public crates/update/update-key.pub
//!       A fresh key pair. Writes the public half to the file (commit it) and
//!       prints the secret half to stdout and nowhere else, so it can be piped
//!       straight into `gh secret set ARBOS_UPDATE_SIGNING_KEY` without ever
//!       landing in a file or a shell history.
//!
//!   arbos-updatectl sign <file>
//!       The base64 signature over a file's bytes, for the release manifest's
//!       per-file `signature` field.
//!
//!   arbos-updatectl verify --signature <base64> [--public <path>] <file>
//!       Whether that signature is this key's. Exits non-zero when it is not.
//!       CI runs it after every `sign` so a repository secret that does not
//!       match the committed public half fails the publish, not every client.
//!
//! Recovered from the legacy tree (commit c5d2c30e,
//! `crates/arbos-update/src/bin/updatectl.rs`); the channel-feed commands that
//! crate also carried died with that tree's feed format — this repository
//! publishes `manifest.json` — and are not resurrected here.
//!
//! The secret is read from `ARBOS_UPDATE_SIGNING_KEY` and never from an
//! argument: an argument is visible to every other process on the machine.

use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};
use zeron_update::sign::{self, PublicKey, SecretKey};

/// The variable CI puts the private half in.
const KEY_ENV: &str = "ARBOS_UPDATE_SIGNING_KEY";

/// Where the public half lives when nothing says otherwise.
const KEY_FILE: &str = "crates/update/update-key.pub";

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
        "keygen" => keygen(&args),
        "sign" => sign_file(&args),
        "verify" => verify_file(&args),
        "" | "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => bail!("no such command `{other}`\n\n{USAGE}"),
    }
}

const USAGE: &str = "\
arbos-updatectl keygen  [--public <path>]
arbos-updatectl sign    <file>
arbos-updatectl verify  --signature <base64> [--public <path>] <file>

The signing key is read from ARBOS_UPDATE_SIGNING_KEY.";

fn keygen(args: &Args) -> Result<()> {
    let path = PathBuf::from(args.one("public").unwrap_or(KEY_FILE.into()));
    let (secret, public) = sign::generate()?;
    let kept = std::fs::read_to_string(&path).unwrap_or_default();
    // The file is mostly the note explaining what it is and how it got there.
    // Keep every comment line and replace only the key.
    let mut body: String = kept
        .lines()
        .filter(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&public.to_line());
    body.push('\n');
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("wrote the public half to {} — commit it", path.display());
    eprintln!("the secret half is on stdout; pipe it into `gh secret set {KEY_ENV}`");
    println!("{}", secret.to_base64());
    Ok(())
}

fn sign_file(args: &Args) -> Result<()> {
    let path = args.only_free("a file to sign")?;
    let bytes = read(&path)?;
    println!("{}", secret_key()?.sign(&bytes)?);
    Ok(())
}

fn verify_file(args: &Args) -> Result<()> {
    let path = args.only_free("a file to verify")?;
    let signature = args.need("signature")?;
    let key = public_key(args)?;
    key.verify(&read(&path)?, &signature)?;
    println!("ok {}", path.display());
    Ok(())
}

fn secret_key() -> Result<SecretKey> {
    let text = std::env::var(KEY_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .with_context(|| {
            format!("{KEY_ENV} is not set — run `arbos-updatectl keygen` once and store it")
        })?;
    SecretKey::from_base64(&text).with_context(|| format!("{KEY_ENV}"))
}

fn public_key(args: &Args) -> Result<PublicKey> {
    let path = args.one("public").unwrap_or(KEY_FILE.into());
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    sign::parse_key_file(&text).with_context(|| format!("{path} holds no `ed25519 <base64>` line"))
}

fn read(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// `--name value`, repeatable, plus whatever is left over.
///
/// Hand-rolled because this tool has three commands and no ambiguity in any
/// of them, and because the kernel's dependency graph should not grow an
/// argument parser for a CI helper.
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

    fn only_free(&self, what: &str) -> Result<PathBuf> {
        match self.free.as_slice() {
            [one] => Ok(PathBuf::from(one)),
            [] => bail!("{what}"),
            many => bail!("{what}, not {}", many.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Args;
    use std::path::PathBuf;

    #[test]
    fn reads_repeated_flags_and_both_spellings() {
        let args = Args::parse(
            [
                "--signature",
                "AAA=",
                "--public=/tmp/k.pub",
                "left-over",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(args.one("signature").as_deref(), Some("AAA="));
        assert_eq!(args.one("public").as_deref(), Some("/tmp/k.pub"));
        assert_eq!(args.all("missing"), Vec::<String>::new());
        assert_eq!(args.free, ["left-over"]);
        assert!(args.need("missing").is_err());
    }

    #[test]
    fn a_flag_with_no_value_is_an_error() {
        let args = Args::parse(["--signature"].into_iter().map(String::from));
        assert!(args.is_err());
    }

    #[test]
    fn only_free_wants_exactly_one_path() {
        let args = Args::parse(["a", "b"].into_iter().map(String::from)).unwrap();
        assert!(args.only_free("a file").is_err());
        let none = Args::parse(Vec::<String>::new().into_iter()).unwrap();
        assert!(none.only_free("a file").is_err());
        let one = Args::parse(["a"].into_iter().map(String::from)).unwrap();
        assert_eq!(one.only_free("a file").unwrap(), PathBuf::from("a"));
    }

    #[test]
    fn a_generated_pair_signs_what_the_publish_flow_signs() {
        // The path CI drives: keygen → sign with the secret from the
        // environment → verify against the public half from a file.
        let (secret, public) = zeron_update::sign::generate().unwrap();
        let payload = b"arbos-0.2.112.4871-linux-x86_64.tar.gz, as bytes";
        let signature = secret.sign(payload).unwrap();
        public.verify(payload, &signature).unwrap();
        assert!(public.verify(b"other bytes", &signature).is_err());
    }
}
