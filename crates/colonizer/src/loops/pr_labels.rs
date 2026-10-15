//! Pull request labels a loop colony asks for: the data-refresh template writes the policy
//! command's verdict (`data-refresh:auto`, `data-refresh:review`) to `/harness/out/pr-labels`, and
//! once the run's pull request exists — opened, or found already open — the mothership applies
//! those labels to it. Only a loop colony's file is read (issue #1037), and nothing here can fail
//! the publish: a label the GitHub token may not create is the run's cosmetic loss.

use crate::sessions::{Session, SessionLogger};
use crate::{App, util::exec};
use std::io::Read as _;
use std::path::Path as FsPath;

/// The most labels one run may ask for.
const MAX_LABELS: usize = 10;
/// The most characters in one label.
const MAX_LABEL: usize = 50;
/// The file holds a handful of lines; anything larger is not a label list.
const MAX_BYTES: u64 = 8 * 1024;

/// The labels `out/pr-labels` asks for: one per line, trimmed, blanks skipped, and anything that
/// is not a short plain label dropped — a comma would split into two labels at `gh --add-label`.
/// A file that is not a plain regular file, or too big, is ignored.
pub(crate) fn read(file: &FsPath) -> Vec<String> {
    // The symlink itself, not its target: what the colony left here must be the label list.
    let Ok(meta) = std::fs::symlink_metadata(file) else {
        return Vec::new();
    };
    if !meta.file_type().is_file() {
        return Vec::new();
    }
    let Ok(list) = std::fs::File::open(file) else {
        return Vec::new();
    };
    // Bounded past the cap by one byte, so a file that grew since the check above still reads as
    // over the cap rather than silently truncated.
    let mut bytes = Vec::new();
    if list.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    if bytes.len() as u64 > MAX_BYTES {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.contains(',')
                && l.chars().count() <= MAX_LABEL
                && !l.chars().any(char::is_control)
        })
        .map(str::to_string)
        .take(MAX_LABELS)
        .collect()
}

/// Applies the run's asked-for labels to the pull request at `url`, best effort: each label is
/// created if it does not exist yet (a failure there is ignored — it may exist already, or the
/// token may not be allowed to create labels), then the labels go onto the pull request. Only a
/// loop colony's run is labeled, and the operator's kill switch holds here too (issue #84).
pub(crate) async fn apply(app: &App, s: &Session, log: &SessionLogger, url: &str) {
    if super::loop_id_of(s.origin.as_deref().unwrap_or_default()).is_none() {
        return;
    }
    if crate::authority::external_writes_blocked() {
        return;
    }
    let labels = read(&app.session_dir(&s.id).join("out").join("pr-labels"));
    if labels.is_empty() {
        return;
    }
    let repo = s.repo.as_str();
    for label in &labels {
        let _ = exec(&mut app.gh(["label", "create", label.as_str(), "-R", repo])).await;
    }
    let mut add = app.gh(["pr", "edit", url, "-R", repo, "--add-label", labels.join(",").as_str()]);
    match exec(&mut add).await {
        Ok(_) => log.info(format!("labeled the pull request: {}", labels.join(", "))).await,
        Err(e) => log.warn(format!("could not label the pull request: {e:#}")).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_one_per_line_bounded_and_sanitized() {
        let dir = std::env::temp_dir().join(format!("colonizer-pr-labels-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pr-labels");

        std::fs::write(&file, "data-refresh:auto\n\n  data-refresh:review  \n").unwrap();
        assert_eq!(
            read(&file),
            ["data-refresh:auto", "data-refresh:review"],
            "blanks skipped, lines trimmed"
        );

        std::fs::write(
            &file,
            (1..=MAX_LABELS + 1)
                .map(|i| format!("label-{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(read(&file).len(), MAX_LABELS, "at most ten labels");

        std::fs::write(&file, format!("{}\nok", "x".repeat(MAX_LABEL + 1))).unwrap();
        assert_eq!(read(&file), ["ok"], "a label over {MAX_LABEL} characters is dropped");

        std::fs::write(&file, "bad\u{7}label\n-ok\nok").unwrap();
        assert_eq!(read(&file), ["ok"], "control characters and flag-looking labels are dropped");

        std::fs::write(&file, "auto,review\nok").unwrap();
        assert_eq!(
            read(&file),
            ["ok"],
            "a comma would split at `gh --add-label`, so the line is dropped"
        );

        std::fs::write(&file, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert!(read(&file).is_empty(), "an oversized file is ignored");
        assert!(read(&dir.join("missing")).is_empty(), "a missing file is no labels");
        assert!(read(&dir).is_empty(), "a directory is not a label list");

        #[cfg(unix)]
        {
            let real = dir.join("real");
            std::fs::write(&real, "data-refresh:auto\n").unwrap();
            let link = dir.join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(read(&link).is_empty(), "a symlink is not the label list");
        }

        let _ = std::fs::remove_dir_all(dir);
    }
}
