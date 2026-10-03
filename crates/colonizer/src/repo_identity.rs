//! One stable identity per repository across a fleet (issue #763, epic #691). Members clone the
//! same repository under different paths and remotes — SSH on one machine, HTTPS on another, a
//! fork or a mirror on a third — so neither the path nor the raw remote says "same repo". The
//! fingerprint is the repository's root commit SHA(s): they survive forks, mirrors and remote
//! renames. A repository with no commits (or one git cannot read) falls back to its normalised
//! origin URL. Matching asks the roots first, then the URL, and never guesses: more than one
//! candidate at the deciding step is `None`. #688 (placement) and #689 (per-repo cost) key on it.
//!
//! Everything here is a pure function except [`read`], a thin reader over the hardened host git
//! (`github::host_git_offline`).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// What a fleet knows a repository by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIdentity {
    /// The root commit SHA(s), lowercase, sorted and deduplicated. More than one when unrelated
    /// histories were merged; empty when the repository has no commits or could not be read.
    #[serde(default)]
    pub roots: Vec<String>,
    /// The origin URL in [`normalize_remote_url`] form — never the raw remote, which can carry a
    /// user name or a token.
    #[serde(default)]
    pub url: Option<String>,
}

impl RepoIdentity {
    /// An identity from raw parts: `rev-list --max-parents=0` output and the raw origin URL.
    pub fn from_parts(rev_list: &str, origin_url: Option<&str>) -> RepoIdentity {
        RepoIdentity {
            roots: parse_roots(rev_list),
            url: origin_url.and_then(normalize_remote_url),
        }
    }

    /// Nothing to identify the repository by.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.url.is_none()
    }

    fn shares_root(&self, other: &RepoIdentity) -> bool {
        self.roots.iter().any(|r| other.roots.contains(r))
    }
}

/// The root SHAs out of `git rev-list --max-parents=0` output: one hex object id per line
/// (SHA-1 or SHA-256), lowercased, sorted, deduplicated; anything else is dropped.
pub fn parse_roots(rev_list: &str) -> Vec<String> {
    let mut roots: Vec<String> = rev_list
        .lines()
        .map(str::trim)
        .filter(|l| matches!(l.len(), 40 | 64) && l.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// A remote URL reduced to `host/path`, so SSH, HTTPS and scp-like spellings of one repository
/// compare equal:
///
/// - the scheme (`https://`, `ssh://`, `git+ssh://`, `git://`) and any `user[:password]@` go;
/// - scp-like `git@host:owner/repo` becomes `host/owner/repo`;
/// - the port goes (`ssh://git@host:2222/…` and `https://host/…` name the same repository);
/// - a trailing `/` and then a trailing `.git` go;
/// - the host is lowercased. The path keeps its case — most git hosts treat it as case-sensitive —
///   except on github.com, where paths are case-insensitive, so the whole URL is lowercased.
///
/// `None` for anything that is not a network remote: local paths and `file://` URLs mean nothing
/// on another machine, and an empty host or path identifies nothing.
pub fn normalize_remote_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (authority_and_path, scp_like) = match raw.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            (rest, false)
        }
        None => {
            // scp-like: a `:` before any `/`, e.g. `git@host:owner/repo`. Anything else without a
            // scheme is a local path.
            let colon = raw.find(':')?;
            if raw[..colon].contains('/') {
                return None;
            }
            (raw, true)
        }
    };

    let (authority, path) = if scp_like {
        let (authority, path) = authority_and_path.split_once(':')?;
        (authority, path)
    } else {
        authority_and_path.split_once('/').unwrap_or((authority_and_path, ""))
    };
    // Userinfo (`user@`, `user:token@`) never survives: it is not identity, and it can be secret.
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if scp_like {
        host
    } else {
        // A `[v6]:port` keeps its brackets; otherwise the port is whatever follows the last `:`.
        match host.rfind(':') {
            Some(i) if !host[i..].contains(']') => &host[..i],
            _ => host,
        }
    };
    let host = match host.to_ascii_lowercase() {
        www if www == "www.github.com" => "github.com".to_string(),
        host => host,
    };

    let mut path = path.trim_matches('/');
    path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let path = if host == "github.com" {
        path.to_ascii_lowercase()
    } else {
        path.to_string()
    };
    Some(format!("{host}/{path}"))
}

/// Which of `candidates` is the same repository as `target`, if exactly one is. In order, the
/// first step with any match decides:
///
/// 1. the same root set;
/// 2. a shared root (a fork that later merged another history still has its original root);
/// 3. the same normalised URL — but never a candidate whose known roots are disjoint from the
///    target's known roots: that is conflicting evidence (a deleted and re-created repository
///    under the old name), not a match.
///
/// More than one candidate at the deciding step is `None`: ambiguous, never a guess.
#[allow(dead_code)] // The placement (#688) and per-repo cost (#689) lookups drive this seam; the unit tests exercise it until then.
pub fn find_match(target: &RepoIdentity, candidates: &[RepoIdentity]) -> Option<usize> {
    fn only(hits: Vec<usize>) -> Result<Option<usize>, ()> {
        match hits.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            _ => Err(()),
        }
    }
    let pick = |keep: &dyn Fn(&RepoIdentity) -> bool| -> Result<Option<usize>, ()> {
        only(
            candidates
                .iter()
                .enumerate()
                .filter(|(_, c)| keep(c))
                .map(|(i, _)| i)
                .collect(),
        )
    };
    let steps: [&dyn Fn(&RepoIdentity) -> bool; 3] = [
        &|c| !target.roots.is_empty() && c.roots == target.roots,
        &|c| target.shares_root(c),
        &|c| {
            let conflicting = !target.roots.is_empty() && !c.roots.is_empty() && !target.shares_root(c);
            target.url.is_some() && c.url == target.url && !conflicting
        },
    ];
    for keep in steps {
        match pick(keep) {
            Ok(Some(i)) => return Some(i),
            Ok(None) => continue,
            Err(()) => return None,
        }
    }
    None
}

/// Reads a local repository's identity (bare mirror or worktree) through the hardened host git:
/// `rev-list --max-parents=0 HEAD` and `config --get remote.origin.url`. A step git cannot answer
/// (no commits, no origin, not a repository) contributes nothing, so the result may be empty.
pub fn read(repo: &Path) -> RepoIdentity {
    let run = |args: &[&str]| -> Option<String> {
        let out = crate::github::host_git_offline()
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let roots = run(&["rev-list", "--max-parents=0", "HEAD"]).unwrap_or_default();
    let url = run(&["config", "--get", "remote.origin.url"]);
    RepoIdentity::from_parts(&roots, url.as_deref().map(str::trim))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("colonizer-repo-identity-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A fresh repository at `root/name` with one commit per entry of `commits`.
    fn repo(root: &Path, name: &str, commits: &[&str]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        for c in commits {
            std::fs::write(dir.join(format!("{c}.txt")), c).unwrap();
            git(&dir, &["add", "-A"]);
            git(&dir, &["commit", "-q", "-m", c]);
        }
        dir
    }

    /// A clone of `src` at `root/name` whose origin is then renamed to `remote`.
    fn clone_as(root: &Path, src: &Path, name: &str, remote: &str) -> PathBuf {
        git(root, &["clone", "-q", src.to_str().unwrap(), name]);
        let dir = root.join(name);
        git(&dir, &["remote", "set-url", "origin", remote]);
        dir
    }

    #[test]
    fn url_normalisation() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "https://github.com/Colonizer-dev/harness.git",
                Some("github.com/colonizer-dev/harness"),
            ),
            (
                "git@github.com:Colonizer-dev/harness.git",
                Some("github.com/colonizer-dev/harness"),
            ),
            (
                "ssh://git@github.com/Colonizer-dev/harness",
                Some("github.com/colonizer-dev/harness"),
            ),
            (
                "ssh://git@github.com:22/Colonizer-dev/harness.git/",
                Some("github.com/colonizer-dev/harness"),
            ),
            ("git+ssh://git@GitHub.com/a/b.git", Some("github.com/a/b")),
            ("https://www.github.com/a/b", Some("github.com/a/b")),
            ("https://x-access-token:ghs_secret@github.com/a/b.git", Some("github.com/a/b")),
            ("https://github.com/a/b/", Some("github.com/a/b")),
            ("  https://github.com/a/b.git\n", Some("github.com/a/b")),
            // Off GitHub the path keeps its case; the host is still lowercased.
            (
                "https://GitLab.Example.com/Group/Sub/Repo.git",
                Some("gitlab.example.com/Group/Sub/Repo"),
            ),
            ("git@GitLab.Example.com:Group/Repo", Some("gitlab.example.com/Group/Repo")),
            (
                "git://git.kernel.org/pub/scm/git/git.git",
                Some("git.kernel.org/pub/scm/git/git"),
            ),
            ("http://[::1]:8080/a/b.git", Some("[::1]/a/b")),
            // Not network remotes, or nothing to identify.
            ("/srv/git/repo.git", None),
            ("./relative/repo", None),
            ("file:///srv/git/repo.git", None),
            ("https://github.com/", None),
            ("git@github.com:", None),
            ("", None),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize_remote_url(raw).as_deref(), *want, "{raw:?}");
        }
    }

    #[test]
    fn roots_are_parsed_sorted_and_filtered() {
        let a = "a".repeat(40);
        let b = "B".repeat(40);
        let sha256 = "c".repeat(64);
        let raw = format!("{b}\n{a}\n\nnot-a-sha\n{a}\n{sha256}\n");
        assert_eq!(parse_roots(&raw), vec![a.clone(), "b".repeat(40), sha256]);
        assert!(parse_roots("").is_empty());
    }

    fn id(roots: &[&str], url: Option<&str>) -> RepoIdentity {
        RepoIdentity {
            roots: roots.iter().map(|r| r.repeat(40)).collect(),
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn matching_table() {
        let web = id(&["a"], Some("github.com/acme/web"));
        let api = id(&["b"], Some("github.com/acme/api"));
        let merged = id(&["a", "c"], Some("github.com/acme/web-plus"));
        let url_only = id(&[], Some("github.com/acme/empty"));
        let cases: &[(&str, RepoIdentity, &[RepoIdentity], Option<usize>)] = &[
            ("exact root set", web.clone(), &[api.clone(), web.clone()], Some(1)),
            (
                "exact set beats a shared root",
                web.clone(),
                &[merged.clone(), web.clone()],
                Some(1),
            ),
            (
                "a shared root alone",
                id(&["a"], None),
                &[api.clone(), merged.clone()],
                Some(1),
            ),
            (
                "roots decide over a different url",
                id(&["b"], Some("github.com/fork/api")),
                &[web.clone(), api.clone()],
                Some(1),
            ),
            (
                "url when roots say nothing",
                id(&[], Some("github.com/acme/empty")),
                &[web.clone(), url_only.clone()],
                Some(1),
            ),
            (
                "url when the candidate has no roots",
                id(&["d"], Some("github.com/acme/empty")),
                std::slice::from_ref(&url_only),
                Some(0),
            ),
            (
                "disjoint roots veto a url match",
                id(&["d"], Some("github.com/acme/web")),
                std::slice::from_ref(&web),
                None,
            ),
            (
                "unrelated",
                id(&["d"], Some("github.com/other/x")),
                &[web.clone(), api.clone()],
                None,
            ),
            (
                "ambiguous exact set",
                web.clone(),
                &[web.clone(), api.clone(), web.clone()],
                None,
            ),
            (
                "ambiguous shared root",
                id(&["a"], None),
                &[merged.clone(), id(&["a", "e"], None)],
                None,
            ),
            (
                "ambiguous url",
                id(&[], Some("github.com/acme/empty")),
                &[url_only.clone(), url_only.clone()],
                None,
            ),
            (
                "nothing to match on",
                RepoIdentity::default(),
                &[RepoIdentity::default()],
                None,
            ),
            ("no candidates", web.clone(), &[], None),
        ];
        for (what, target, candidates, want) in cases {
            assert_eq!(find_match(target, candidates), *want, "{what}");
        }
    }

    #[test]
    fn the_same_repo_over_ssh_and_https_is_one_identity() {
        let root = scratch("ssh-https");
        let src = repo(&root, "src", &["one", "two"]);
        let over_ssh = read(&clone_as(&root, &src, "ssh", "git@github.com:Acme/Web.git"));
        let over_https = read(&clone_as(&root, &src, "https", "https://github.com/acme/web"));
        assert_eq!(over_ssh.roots.len(), 1);
        assert_eq!(over_ssh, over_https, "same roots and the same normalised url");
        assert_eq!(over_ssh.url.as_deref(), Some("github.com/acme/web"));
        assert_eq!(find_match(&over_ssh, std::slice::from_ref(&over_https)), Some(0));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_fork_with_its_own_remote_matches_its_upstream_by_root() {
        let root = scratch("fork");
        let src = repo(&root, "src", &["one"]);
        let upstream = read(&clone_as(&root, &src, "upstream", "https://github.com/acme/web.git"));
        let fork_dir = clone_as(&root, &src, "fork", "git@github.com:someone/web-fork.git");
        // The fork moves on; its root does not.
        std::fs::write(fork_dir.join("fork.txt"), "mine").unwrap();
        git(&fork_dir, &["add", "-A"]);
        git(&fork_dir, &["commit", "-q", "-m", "fork work"]);
        let fork = read(&fork_dir);
        assert_ne!(fork.url, upstream.url);
        assert_eq!(fork.roots, upstream.roots);
        let other = read(&repo(&root, "other", &["x"]));
        assert_eq!(find_match(&fork, &[other, upstream]), Some(1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_unrelated_repos_never_match() {
        let root = scratch("unrelated");
        // Different first commits: two byte-identical root commits (same tree, message, author
        // and second) are the same object, and would rightly count as one history.
        let a = read(&repo(&root, "a", &["one"]));
        let b = read(&repo(&root, "b", &["another"]));
        assert_eq!(a.url, None, "a repo without an origin has roots only");
        assert_ne!(a.roots, b.roots);
        assert_eq!(find_match(&a, std::slice::from_ref(&b)), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_repo_with_merged_unrelated_histories_carries_every_root() {
        let root = scratch("multi-root");
        let dir = repo(&root, "multi", &["one"]);
        let first = parse_roots(&git(&dir, &["rev-list", "--max-parents=0", "HEAD"]));
        git(&dir, &["checkout", "-q", "--orphan", "other"]);
        git(&dir, &["rm", "-rq", "--cached", "."]);
        std::fs::remove_file(dir.join("one.txt")).unwrap();
        std::fs::write(dir.join("two.txt"), "two").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "second root"]);
        let second = parse_roots(&git(&dir, &["rev-parse", "HEAD"]));
        git(&dir, &["checkout", "-q", "main"]);
        git(&dir, &["merge", "-q", "--allow-unrelated-histories", "-m", "join", "other"]);

        let multi = read(&dir);
        let mut want = [first, second].concat();
        want.sort();
        assert_eq!(multi.roots, want, "both roots, sorted");
        // The single-root original still finds the merged repository by the root they share.
        let single = RepoIdentity {
            roots: vec![want[0].clone()],
            url: None,
        };
        assert_eq!(find_match(&single, std::slice::from_ref(&multi)), Some(0));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_ambiguous_match_is_none() {
        let root = scratch("ambiguous");
        let src = repo(&root, "src", &["one"]);
        let target = read(&clone_as(&root, &src, "target", "https://github.com/acme/web"));
        let mirror_a = read(&clone_as(&root, &src, "mirror-a", "https://mirror-a.example/acme/web"));
        let mirror_b = read(&clone_as(&root, &src, "mirror-b", "https://mirror-b.example/acme/web"));
        assert_eq!(
            find_match(&target, &[mirror_a, mirror_b]),
            None,
            "two candidates share the roots"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_empty_or_missing_repo_reads_as_what_it_has() {
        let root = scratch("empty");
        let empty = repo(&root, "empty", &[]);
        git(&empty, &["remote", "add", "origin", "git@github.com:acme/empty.git"]);
        assert_eq!(read(&empty), id(&[], Some("github.com/acme/empty")));
        assert!(read(&root.join("missing")).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
