fn main() {
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
