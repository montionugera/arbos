use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    // The build number is a compile-time constant (see `update::current_version`);
    // a change in the environment must rebuild the crate.
    println!("cargo:rerun-if-env-changed=ARBOS_BUILD");
    embed_tree_avatars();
}

/// Tree-character avatars (assets/avatars/trees, produced on `cursor/avatars-trees`):
/// `manifest.json` plus pre-rendered frame strips `frames/<slug>-<state>[-rail][@2x].png`.
/// Embedded here so the app has no asset path at runtime; an absent directory still
/// builds (empty table) so CI before the assets land stays green.
fn embed_tree_avatars() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest_dir.join("../../assets/avatars/trees");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("tree_avatars.rs");
    println!("cargo:rerun-if-changed={}", root.display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("manifest.json").display()
    );
    println!("cargo:rerun-if-changed={}", root.join("frames").display());

    let manifest = fs::read_to_string(root.join("manifest.json")).unwrap_or_default();
    let mut strips: Vec<(String, String, bool, u32, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("frames")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".png") else {
                continue;
            };
            let (stem, scale) = match stem.strip_suffix("@2x") {
                Some(s) => (s, 2u32),
                None => (stem, 1u32),
            };
            let (stem, rail) = match stem.strip_suffix("-rail") {
                Some(s) => (s, true),
                None => (stem, false),
            };
            let Some((slug, state)) = stem.rsplit_once('-') else {
                continue;
            };
            if !matches!(state, "idle" | "working" | "reply") {
                continue;
            }
            strips.push((slug.to_string(), state.to_string(), rail, scale, path));
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    strips.sort();
    let mut code = String::new();
    code.push_str("/// manifest.json verbatim (empty when the assets are not checked out).\n");
    code.push_str(&format!(
        "pub static MANIFEST_JSON: &str = {:?};\n",
        manifest
    ));
    code.push_str("/// (slug, state, rail, scale, png bytes)\n");
    code.push_str("pub static STRIPS: &[(&str, &str, bool, u32, &[u8])] = &[\n");
    for (slug, state, rail, scale, path) in &strips {
        code.push_str(&format!(
            "    ({:?}, {:?}, {}, {}, include_bytes!({:?})),\n",
            slug,
            state,
            rail,
            scale,
            abs(path)
        ));
    }
    code.push_str("];\n");
    fs::write(&out, code).expect("write tree_avatars.rs");
}

fn abs(p: &Path) -> String {
    fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
