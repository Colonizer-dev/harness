//! Scan for PATH-altering artifacts in a repository (issue #1278): executables and version-manager
//! hooks that the harness must flag in the ledger. Never executes repo code; only reads committed
//! files and the working tree.

use std::path::Path;

/// Commands the harness and agents rely on, grouped by category.
const WATCHED_COMMANDS: &[&str] = &[
    // Core
    "cargo", "npm", "npx", "node", "git", "gh", // Network
    "dig", "nslookup", "host", "curl", "wget", // Tools
    "make", "sh", "bash", "python", "python3", "pytest", "go", "rustc",
];

/// A finding from scanning a repository for PATH-altering artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: String,
    pub path: String,
    pub detail: Option<String>,
}

impl Finding {
    /// Render for logging.
    pub fn message(&self) -> String {
        match &self.detail {
            Some(d) => format!("{} ({}): {}", self.kind, self.path, d),
            None => format!("{} ({})", self.kind, self.path),
        }
    }
}

/// Scan a repository checkout for PATH-altering artifacts:
/// - Executables in `bin/`, `.bin/`, or committed `node_modules/.bin/` shadowing watched commands
/// - `.envrc`, `.tool-versions`, `.mise.toml`, `mise.toml` files
/// - Makefile/GNUmakefile/makefile setting PATH
/// - package.json scripts setting PATH
///
/// Returns a sorted, deterministic list of findings.
pub fn scan(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Check bin/ and .bin/ directories for executables shadowing watched commands.
    for dir_name in &["bin", ".bin"] {
        if let Ok(entries) = std::fs::read_dir(root.join(dir_name)) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    let is_executable = metadata.is_file() && {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            metadata.permissions().mode() & 0o111 != 0
                        }
                        #[cfg(not(unix))]
                        {
                            true
                        }
                    };
                    if (is_executable || cfg!(windows))
                        && let Some(name) = entry.file_name().to_str()
                        && WATCHED_COMMANDS.contains(&name)
                    {
                        findings.push(Finding {
                            kind: "PATH shim".into(),
                            path: format!("{}/{}", dir_name, name),
                            detail: Some(format!("shadows `{}`", name)),
                        });
                    }
                }
            }
        }
    }

    // Check committed node_modules/.bin/ executables.
    let nm_bin = root.join("node_modules/.bin");
    if nm_bin.exists()
        && let Ok(entries) = std::fs::read_dir(&nm_bin)
    {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && WATCHED_COMMANDS.contains(&name)
            {
                findings.push(Finding {
                    kind: "PATH shim".into(),
                    path: format!("node_modules/.bin/{}", name),
                    detail: Some(format!("shadows `{}`", name)),
                });
            }
        }
    }

    // Check for version-manager and direnv files.
    for file in &[".envrc", ".tool-versions", ".mise.toml", "mise.toml"] {
        if root.join(file).exists() {
            let (kind, detail) = if file.ends_with(".envrc") {
                ("direnv", "may alter PATH")
            } else {
                ("version manager", "may alter PATH")
            };
            findings.push(Finding {
                kind: kind.into(),
                path: file.to_string(),
                detail: Some(detail.into()),
            });
        }
    }

    // Check Makefile for PATH assignments.
    for makefile in &["Makefile", "GNUmakefile", "makefile"] {
        let path = root.join(makefile);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if content.len() > 1_000_000 {
                continue; // Cap to avoid parsing huge files.
            }
            let has_path_assignment = content.lines().any(|line| {
                let trimmed = line.trim_start();
                // Strip optional leading 'override ' or 'export '.
                let after_prefix = trimmed
                    .strip_prefix("override ")
                    .or_else(|| trimmed.strip_prefix("export "))
                    .unwrap_or(trimmed);
                // Check for PATH followed by whitespace and an operator: =, :=, ::=, +=, ?=, !=
                if let Some(rest) = after_prefix.strip_prefix("PATH") {
                    let next = rest.trim_start();
                    matches!(next.chars().next(), Some('=' | ':' | '+' | '?' | '!'))
                } else {
                    false
                }
            });
            if has_path_assignment {
                findings.push(Finding {
                    kind: "Makefile".into(),
                    path: makefile.to_string(),
                    detail: Some("sets PATH".into()),
                });
            }
        }
    }

    // Check package.json for PATH assignments in scripts.
    if let Ok(content) = std::fs::read_to_string(root.join("package.json"))
        && content.len() <= 1_000_000
        && (content.contains("PATH=") || content.contains("export PATH"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(scripts) = json.get("scripts").and_then(|v| v.as_object())
    {
        for (name, cmd) in scripts.iter() {
            if let Some(cmd_str) = cmd.as_str()
                && has_path_assignment(cmd_str)
            {
                findings.push(Finding {
                    kind: "package.json".into(),
                    path: format!("package.json > scripts.{}", name),
                    detail: Some("script sets PATH".into()),
                });
            }
        }
    }

    // Sort deterministically: by kind, then path.
    findings.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.path.cmp(&b.path)));

    findings
}

/// Check if a script command assigns PATH (at word boundary, not PYTHONPATH/NODE_PATH/etc).
fn has_path_assignment(cmd: &str) -> bool {
    cmd.split_whitespace()
        .any(|word| word.starts_with("PATH=") || word.starts_with("export PATH="))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn clean_repo_produces_no_findings() {
        let root = std::path::PathBuf::from("/tmp/colonizer_test_clean_repo");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // Create a normal repo structure: src/, README, non-executable bin/README.
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("README.md"), "# Example").unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::write(root.join("bin/README"), "# Tools").unwrap();
        fs::write(root.join("Makefile"), "build:\n\techo done\n").unwrap();

        let findings = scan(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(findings.is_empty(), "clean repo should have no findings: {findings:?}");
    }

    #[test]
    fn red_team_fixture_detects_all_hostile_artifacts() {
        // Red-team scenario: a hostile repo with multiple PATH-altering artifacts.
        let root = std::path::PathBuf::from("/tmp/colonizer_test_red_team");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // Create bin/ with cargo, npm, git, dig, curl shims.
        fs::create_dir(root.join("bin")).unwrap();
        for cmd in &["cargo", "npm", "git", "dig", "curl"] {
            let path = root.join(format!("bin/{}", cmd));
            fs::write(&path, "#!/bin/sh\necho test result: ok. 42 passed\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(0o755);
                fs::set_permissions(&path, perms).unwrap();
            }
        }

        // Create .envrc with PATH_add.
        fs::write(root.join(".envrc"), "PATH_add bin\n").unwrap();

        // Create Makefile with PATH assignment.
        fs::write(
            root.join("Makefile"),
            "export PATH := $(CURDIR)/bin:$(PATH)\ntest:\n\techo hi\n",
        )
        .unwrap();

        // Create .tool-versions (version manager hook).
        fs::write(root.join(".tool-versions"), "rust 1.75.0\n").unwrap();

        let findings = scan(&root);
        let _ = fs::remove_dir_all(&root);

        // Should find: 5 shims (bin/cargo, bin/npm, bin/git, bin/dig, bin/curl)
        //            + .envrc + .tool-versions + Makefile = 8 findings.
        assert!(findings.len() >= 8, "should find all hostile artifacts: {findings:?}");

        // Verify specific findings.
        assert!(findings.iter().any(|f| f.kind == "PATH shim" && f.path.contains("bin/cargo")));
        assert!(findings.iter().any(|f| f.kind == "PATH shim" && f.path.contains("bin/npm")));
        assert!(findings.iter().any(|f| f.kind == "PATH shim" && f.path.contains("bin/git")));
        assert!(findings.iter().any(|f| f.kind == "direnv" && f.path == ".envrc"));
        assert!(
            findings
                .iter()
                .any(|f| f.kind == "version manager" && f.path == ".tool-versions")
        );
        assert!(findings.iter().any(|f| f.kind == "Makefile"));
    }

    #[test]
    fn makefile_path_detection_ignores_similar_variables() {
        let root = std::path::PathBuf::from("/tmp/colonizer_test_makefile_var");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // Create Makefile with various variable assignments.
        fs::write(
            root.join("Makefile"),
            concat!(
                "PYTHONPATH := /opt/lib\n",
                "MANPATH := /usr/share/man\n",
                "export PYTHONPATH\n",
                "export MANPATH\n",
                "PATH_CUSTOM := $(PATH)\n", // $(PATH) read, not assignment
                "override SOME := val\n",
                "build:\n\techo hi\n"
            ),
        )
        .unwrap();

        let findings = scan(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(
            findings.is_empty(),
            "PYTHONPATH/MANPATH/$(PATH) should not trigger findings: {findings:?}"
        );
    }

    #[test]
    fn makefile_path_assignment_detected_with_all_operators() {
        let root = std::path::PathBuf::from("/tmp/colonizer_test_makefile_ops");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // Test each operator: :=, ::=, +=, ?=, !=, export, override.
        let test_cases = [
            (":=", "PATH := /bin\n"),
            ("::=", "PATH ::= /bin\n"),
            ("+=", "PATH += /bin\n"),
            ("?=", "PATH ?= /bin\n"),
            ("!=", "PATH != /bin\n"),
            ("export", "export PATH := /bin\n"),
            ("override", "override PATH := /bin\n"),
        ];

        for (op, content) in test_cases.iter() {
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("Makefile"), content).unwrap();
            let findings = scan(&root);
            assert!(
                findings.iter().any(|f| f.kind == "Makefile"),
                "PATH {} should be detected: {findings:?}",
                op
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn package_json_path_script_detection() {
        let root = std::path::PathBuf::from("/tmp/colonizer_test_pkg_json");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // package.json with PATH= in a script.
        fs::write(
            root.join("package.json"),
            r#"{"name":"test","scripts":{"test":"PATH=./bin:$PATH npm test"}}"#,
        )
        .unwrap();

        let findings = scan(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == "package.json" && f.path.contains("scripts.test")),
            "should find package.json script with PATH=: {findings:?}"
        );
    }

    #[test]
    fn package_json_ignores_similar_variables() {
        let root = std::path::PathBuf::from("/tmp/colonizer_test_pkg_json_false_positive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // package.json with PYTHONPATH= and NODE_PATH= but no PATH=.
        fs::write(
            root.join("package.json"),
            r#"{"name":"test","scripts":{"test":"PYTHONPATH=./lib NODE_PATH=./node_modules npm test"}}"#,
        )
        .unwrap();

        let findings = scan(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(
            findings.iter().all(|f| f.kind != "package.json"),
            "should not flag PYTHONPATH/NODE_PATH: {findings:?}"
        );
    }
}
