//! What this build is, and how versions compare. build.rs records `git describe` output, the commit,
//! the dirty flag and the build time as `COLONIZER_BUILD_*`; this module turns those into the one
//! `Build` the API reports (`/api/version`, and the update check's `installed`), and parses release
//! tags closely enough to say whether one is newer than the build that is running.

use crate::Shared;
use axum::{Json, extract::State};
use serde_json::{Value, json};
use std::sync::OnceLock;

pub struct Build {
    /// What `git describe` said, e.g. `v0.1.3` or `v0.1.3-12-gabc1234`; a build with no git answer
    /// (crates.io) reports the crate version instead.
    pub version: String,
    /// The release tag this build contains, e.g. `v0.1.3` even for a build twelve commits past it.
    /// `None` only when the describe string names no tag at all (a bare `--always` sha).
    pub release: Option<String>,
    pub commit: Option<String>,
    pub dirty: bool,
    pub built_at: Option<String>,
    /// True when this is not exactly a tagged release: ahead of its tag, built from a dirty tree, or
    /// never on a tag.
    pub development: bool,
}

/// What this build is, computed once. The `COLONIZER_BUILD_*` variables come from build.rs, which
/// leaves them empty when git could not answer.
pub fn build() -> &'static Build {
    static BUILD: OnceLock<Build> = OnceLock::new();
    BUILD.get_or_init(|| {
        from_describe(
            option_env!("COLONIZER_BUILD_VERSION").unwrap_or_default(),
            option_env!("COLONIZER_BUILD_DIRTY") == Some("1"),
        )
    })
}

/// Fills a `Build` from `git describe` output and the dirty flag, so the derivation can be tested
/// without a rebuild; the commit and build time are only there when build.rs could record them.
fn from_describe(describe: &str, dirty: bool) -> Build {
    let crate_version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let parsed = (!describe.is_empty()).then(|| parse(describe)).flatten();
    let release = parsed
        .as_ref()
        .map(Version::tag)
        .or_else(|| describe.is_empty().then_some(crate_version.clone()));
    Build {
        version: if describe.is_empty() {
            crate_version
        } else {
            describe.to_string()
        },
        release,
        commit: recorded(option_env!("COLONIZER_BUILD_COMMIT")),
        dirty,
        built_at: recorded(option_env!("COLONIZER_BUILD_TIME")),
        // Ahead of its tag, modified after it, or never on a tag: none of those is a release.
        development: dirty
            || parsed.as_ref().is_some_and(|v| v.distance > 0)
            || (!describe.is_empty() && parsed.is_none()),
    }
}

/// A value build.rs recorded, unless it recorded the empty string it uses for "git could not say".
fn recorded(value: Option<&'static str>) -> Option<String> {
    value.map(str::to_string).filter(|v| !v.is_empty())
}

/// The six fields the API reports about a build, shared by `/api/version` and the update view.
pub fn value(build: &Build) -> Value {
    json!({
        "version": build.version,
        "release": build.release,
        "commit": build.commit,
        "dirty": build.dirty,
        "built_at": build.built_at,
        "development": build.development,
    })
}

/// `GET /api/version` — what this build is.
pub async fn status(State(_app): State<Shared>) -> Json<Value> {
    Json(value(build()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The prerelease identifier (`rc.1` in `v0.2.0-rc.1`), without git's distance suffix.
    pub pre: Option<String>,
    /// How far `git describe` says this build is past its tag.
    pub distance: u32,
}

impl Version {
    /// The release tag this version names, e.g. `v0.1.3` or `v0.2.0-rc.1`: the describe distance is
    /// not part of a tag.
    pub fn tag(&self) -> String {
        match &self.pre {
            Some(pre) => format!("v{}.{}.{}-{pre}", self.major, self.minor, self.patch),
            None => format!("v{}.{}.{}", self.major, self.minor, self.patch),
        }
    }

    /// Whether this version sorts after `other`; `Ord` is the whole comparison.
    pub fn is_newer_than(&self, other: &Version) -> bool {
        self > other
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            // A prerelease ships before the release it names; two prereleases compare as plain
            // strings, which is right while identifiers stay simple.
            .then_with(|| match (&self.pre, &other.pre) {
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (a, b) => a.cmp(b),
            })
            // Two builds of one tag order by how far ahead of it they are, so a dev build is never
            // told that the tag it already contains is an update.
            .then_with(|| self.distance.cmp(&other.distance))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Parses a version the way release tags and `git describe` write them: an optional leading `v`,
/// `X.Y.Z`, an optional prerelease, and git's `-<distance>-g<sha>` suffix, which is not a prerelease.
pub fn parse(s: &str) -> Option<Version> {
    let s = s.trim().strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('-');
    let (major, minor, patch) = numbers(parts.next()?)?;
    let middle: Vec<&str> = parts.collect();
    // git describe appends `-<distance>-g<sha>`: a plain count, then a `g` and a hex sha. That pair
    // is recognised here so it is never mistaken for a prerelease named like a sha.
    let described = middle.len() >= 2
        && middle[middle.len() - 1].len() > 1
        && middle[middle.len() - 1].starts_with('g')
        && middle[middle.len() - 2].chars().all(|c| c.is_ascii_digit());
    let (pre, distance) = if described {
        let distance = middle[middle.len() - 2].parse::<u32>().ok()?;
        (
            Some(middle[..middle.len() - 2].join("-")).filter(|p| !p.is_empty()),
            distance,
        )
    } else {
        (Some(middle.join("-")).filter(|p| !p.is_empty()), 0)
    };
    Some(Version {
        major,
        minor,
        patch,
        pre,
        distance,
    })
}

/// Exactly three numeric components, e.g. `0.1.3`.
fn numbers(part: &str) -> Option<(u64, u64, u64)> {
    let mut numbers = part.split('.');
    let (Some(major), Some(minor), Some(patch), None) = (
        numbers.next().and_then(|n| n.parse().ok()),
        numbers.next().and_then(|n| n.parse().ok()),
        numbers.next().and_then(|n| n.parse().ok()),
        numbers.next(),
    ) else {
        return None;
    };
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_with_and_without_the_leading_v() {
        let v = parse("v0.1.3").unwrap();
        assert_eq!(
            (v.major, v.minor, v.patch, v.pre.as_deref(), v.distance),
            (0, 1, 3, None, 0)
        );
        assert_eq!(parse("0.1.3"), parse("v0.1.3"));
        assert_eq!(parse("v0.1.3").unwrap().tag(), "v0.1.3");
    }

    #[test]
    fn describe_distance_is_not_a_prerelease() {
        let v = parse("v0.1.3-12-gabc1234").unwrap();
        assert_eq!(
            (v.major, v.minor, v.patch, v.pre.as_deref(), v.distance),
            (0, 1, 3, None, 12)
        );
        let v = parse("v0.1.3-rc.1-4-gdeadbee").unwrap();
        assert_eq!((v.pre.as_deref(), v.distance), (Some("rc.1"), 4));
        assert_eq!(parse("v0.1.3-12-gabc1234").unwrap().tag(), "v0.1.3");
        assert_eq!(
            parse("v0.1.3-rc.1-4-gdeadbee").unwrap().tag(),
            "v0.1.3-rc.1"
        );
    }

    #[test]
    fn prereleases_keep_their_identifiers() {
        let v = parse("v0.2.0-rc.1").unwrap();
        assert_eq!(
            (v.major, v.minor, v.patch, v.pre.as_deref(), v.distance),
            (0, 2, 0, Some("rc.1"), 0)
        );
        assert_eq!(v.tag(), "v0.2.0-rc.1");
    }

    #[test]
    fn garbage_does_not_parse() {
        for bad in [
            "", "v", "garbage", "gabc1234", "v1", "v1.2", "v1.2.3.4", "vx.y.z", "0.1.x",
        ] {
            assert!(parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_prerelease_sorts_before_the_release_it_names() {
        assert!(parse("v0.1.3-rc.1").unwrap() < parse("v0.1.3").unwrap());
        assert!(parse("v0.1.3-rc.1").unwrap() < parse("v0.1.3-rc.2").unwrap());
        assert!(parse("v0.1.2").unwrap() < parse("v0.1.3-rc.1").unwrap());
        assert!(parse("v0.1.3").unwrap() < parse("v0.2.0").unwrap());
        assert!(
            parse("v0.9.0").unwrap() < parse("v0.10.0").unwrap(),
            "components compare numerically"
        );
        assert_eq!(parse("v0.1.3").unwrap(), parse("0.1.3").unwrap());
    }

    #[test]
    fn a_build_ahead_of_its_tag_sorts_after_the_tag_itself() {
        let tag = parse("v0.1.3").unwrap();
        let ahead = parse("v0.1.3-12-gabc1234").unwrap();
        assert!(ahead > tag, "distance breaks ties towards newer");
        assert!(ahead.is_newer_than(&tag));
        assert!(!tag.is_newer_than(&tag));
        assert!(parse("v0.1.3-12-gabc").unwrap() > parse("v0.1.3-2-gabc").unwrap());
    }

    #[test]
    fn describe_strings_become_a_release_and_a_development_flag() {
        let b = from_describe("v0.1.3", false);
        assert_eq!(b.version, "v0.1.3");
        assert_eq!(b.release.as_deref(), Some("v0.1.3"));
        assert!(!b.development, "exactly on the tag is a release build");

        let b = from_describe("v0.1.3-12-gabc1234", false);
        assert_eq!(b.version, "v0.1.3-12-gabc1234");
        assert_eq!(
            b.release.as_deref(),
            Some("v0.1.3"),
            "the release is the tag the build contains"
        );
        assert!(b.development, "ahead of the tag is a development build");

        let b = from_describe("v0.1.3", true);
        assert!(b.development, "a dirty tree is a development build");
        assert_eq!(b.release.as_deref(), Some("v0.1.3"));

        let b = from_describe("gabc1234", false);
        assert!(b.development, "no tag at all is a development build");
        assert_eq!(b.release, None, "a bare sha names no release");

        let b = from_describe("", false);
        assert_eq!(
            b.release.as_deref(),
            Some(concat!("v", env!("CARGO_PKG_VERSION"))),
            "no describe falls back to the crate version"
        );
        assert!(
            !b.development,
            "a build of a released version without git is a release build"
        );
    }

    #[test]
    fn the_version_report_has_six_fields() {
        let value = value(build());
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "built_at",
                "commit",
                "development",
                "dirty",
                "release",
                "version"
            ]
        );
        assert_eq!(value["dirty"], build().dirty);
        assert_eq!(value["version"], build().version);
    }
}
