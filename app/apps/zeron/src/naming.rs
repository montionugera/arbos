//! The app is Arbos. A string a person can read must not say Zeron.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Source trees whose string literals a person can see.
    const SCANNED: [&str; 4] = [
        "crates/ui/src",
        "crates/theme/src",
        "crates/update/src",
        "apps/zeron/src",
    ];

    /// Lines that may keep the old name: the deep-link scheme, the parser for
    /// the model-facing side-chat header, the HTTP user agent, a persisted
    /// settings key, native class names, the Windows data-directory name
    /// (renaming it would strand a user's data), and panics or logs nobody
    /// reads in the window.
    const ALLOWED: [&str; 12] = [
        "zeron://",
        "[Message from Zeron chat ",
        "Reply to it with the Zeron `send_message` tool",
        "Zeron/desktop",
        "openWebLinksInZeron",
        "ZeronNotifyDelegate",
        "#[name = ",
        ".expect(",
        "tracing::",
        "Zeron.app",
        r"Programs\Zeron",
        "local.join(\"Zeron\")",
    ];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                // This file: the allowlist itself is full of the old name.
                && path.file_name().is_some_and(|name| name != "naming.rs")
            {
                out.push(path);
            }
        }
    }

    /// True when the first `Zeron` on the line sits inside a string literal:
    /// an odd number of unescaped quotes come before it.
    fn inside_a_literal(line: &str) -> bool {
        let Some(at) = line.find("Zeron") else {
            return false;
        };
        let mut quotes = 0;
        let mut escaped = false;
        for ch in line[..at].chars() {
            match ch {
                '\\' => escaped = !escaped,
                '"' if !escaped => quotes += 1,
                _ => escaped = false,
            }
        }
        quotes % 2 == 1
    }

    /// Lines a person could read, after the allowlist and the test modules
    /// are taken out.
    fn visible_zeron_lines(text: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut found = Vec::new();
        let mut in_tests = false;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                let next = lines[index + 1..]
                    .iter()
                    .map(|line| line.trim_start())
                    .find(|line| !line.is_empty());
                if next.is_some_and(|line| line.starts_with("mod ")) {
                    in_tests = true;
                }
            }
            if in_tests || trimmed.starts_with("//") || !line.contains("Zeron") {
                continue;
            }
            if ALLOWED.iter().any(|allowed| line.contains(allowed)) {
                continue;
            }
            if inside_a_literal(line) {
                found.push((index + 1, trimmed.to_owned()));
            }
        }
        found
    }

    #[test]
    fn literal_detection_counts_quotes() {
        assert!(inside_a_literal(r#"let s = "Quit Zeron";"#));
        assert!(inside_a_literal(r#"format!("Update Zeron on {name}")"#));
        assert!(!inside_a_literal("Self::Zeron => Self::Zeron,"));
        assert!(!inside_a_literal(r#"let x = "a"; Zeron"#));
        assert!(inside_a_literal(r#"let x = "say \"Zeron\"";"#));
    }

    #[test]
    fn scanner_skips_comments_tests_and_the_allowlist() {
        let text = "\
// Zeron in a comment\n\
let a = \"Open in Zeron\";\n\
let b = \"zeron://open/chat/\";\n\
let c = Self::Zeron;\n\
#[cfg(test)]\n\
mod tests {\n\
    let d = \"Zeron in a test\";\n\
}\n";
        let found = visible_zeron_lines(text);
        assert_eq!(found, vec![(2, "let a = \"Open in Zeron\";".to_owned())]);
    }

    #[test]
    fn no_string_a_person_reads_says_zeron() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let mut files = Vec::new();
        for tree in SCANNED {
            rust_files(&app.join(tree), &mut files);
        }
        assert!(!files.is_empty(), "no source files under {}", app.display());
        files.sort();
        let mut report = Vec::new();
        for file in &files {
            let text = fs::read_to_string(file).unwrap();
            for (line, text) in visible_zeron_lines(&text) {
                let shown = file.strip_prefix(&app).unwrap_or(file);
                report.push(format!("{}:{line}: {text}", shown.display()));
            }
        }
        assert!(
            report.is_empty(),
            "these strings still say Zeron; say Arbos or add the line to ALLOWED:\n{}",
            report.join("\n")
        );
    }
}
