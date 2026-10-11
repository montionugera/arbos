fn main() {
    emit_arbos_version();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../dist/macos/Info-unbundled.plist");
        println!("cargo:rerun-if-changed={}", plist.display());
        // `cargo run` has no .app Info.plist. NSBundle also reads privacy
        // declarations from this Mach-O section, so source builds can request
        // microphone access without requiring a separate packaging step.
        println!(
            "cargo:rustc-link-arg-bin=zeron=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
    println!("cargo:rerun-if-changed=../../dist/windows/zeron.rc");
    println!("cargo:rerun-if-changed=../../dist/windows/zeron.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for(
            "../../dist/windows/zeron.rc",
            &["zeron"],
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("Windows app icon resource compilation failed");
    }
}

fn emit_arbos_version() {
    println!("cargo:rerun-if-env-changed=ARBOS_BUILD");
    let pkg = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let version = match std::env::var("ARBOS_BUILD") {
        Ok(build) => {
            let build = build.trim();
            if build.is_empty() {
                pkg
            } else if build.bytes().all(|byte| byte.is_ascii_digit()) {
                format!("{pkg}.{build}")
            } else {
                panic!("ARBOS_BUILD must be a decimal build number");
            }
        }
        Err(_) => pkg,
    };
    println!("cargo:rustc-env=ARBOS_VERSION={version}");
}
