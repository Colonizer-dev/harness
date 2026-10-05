//! The exposition itself: how a label value is made safe, how a family is held and printed, and
//! how an org becomes a name a backend cannot learn.
//!
//! Everything here is pure: it takes owned data and returns a `String`, or reads the hash key from
//! disk. Nothing reaches back into the app, so [`super`] owns the state and this file owns the
//! format.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    path::Path,
};

use crate::util;

use super::{HistogramSnapshot, LE_LABELS, MAX_LABEL, OTHER, SERIES_CAP, merge};

// ---------------------------------------------------------------------------
// Label values.
// ---------------------------------------------------------------------------

/// A label value fit to be printed: the safe charset, capped at [`MAX_LABEL`] characters, with the
/// three characters the exposition escapes anyway escaped regardless.
///
/// The charset restriction is what actually protects the output — a quote, a backslash and a
/// newline all become `_` here, before the escape is ever reached — so this is belt and braces on
/// purpose, for the day the safe set is widened.
pub(super) fn label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_LABEL));
    for c in raw.chars().take(MAX_LABEL) {
        out.push(if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':') {
            c
        } else {
            '_'
        });
    }
    escape(&out)
}

/// The exposition's three escapes, applied to an already-sanitised value.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// The `le` label of the bucket every histogram ends with.
pub(super) const LE_INF: &str = "+Inf";

/// How an org reaches a series: as itself, or as this install's keyed hash of it.
pub(super) struct Hasher {
    hashed: bool,
    key: Option<Vec<u8>>,
}

impl Hasher {
    /// A hasher that either passes orgs through or hashes them with `key`, which is `None` when
    /// the key could not be read or created.
    pub(super) fn new(hashed: bool, key: Option<Vec<u8>>) -> Self {
        Self { hashed, key }
    }

    /// The org label for `org`: the name itself, or `hmac-sha256(key, org)[..12]` in hex when
    /// `repo_names` is `hashed`.
    ///
    /// A hashed install whose key could not be read or created reports `unknown` for every org
    /// rather than falling back to plaintext: the switch asks for a name a backend cannot learn, and
    /// quietly sending it anyway would be worse than sending none.
    pub(super) fn org(&self, org: &str) -> String {
        if !self.hashed {
            return label(org);
        }
        match self.key.as_deref() {
            Some(key) => {
                let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
                let tag = ring::hmac::sign(&key, org.as_bytes());
                label(&util::hex(tag.as_ref().get(..12).unwrap_or(tag.as_ref())))
            }
            None => label("unknown"),
        }
    }
}

/// Reads the per-install hash key, creating it at mode 0600 when absent, or `None` when it can be
/// neither read nor written. Blocking: the caller runs it in `spawn_blocking`.
pub(super) fn load_or_create_key(data_dir: &Path) -> Option<Vec<u8>> {
    const KEY_LEN: usize = 32;
    let dir = data_dir.join(crate::observability::state::DIR);
    let path = dir.join("hash.key");
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() == KEY_LEN
    {
        return Some(bytes);
    }
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut key = [0u8; KEY_LEN];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut key).is_err()
        || std::fs::create_dir_all(&dir).is_err()
    {
        return None;
    }
    // Mode on the open, not a chmod after: a key is briefly world-readable otherwise.
    let write = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path);
    if write.is_err() || write.expect("opened").write_all(&key).is_err() {
        return None;
    }
    Some(key.to_vec())
}

// ---------------------------------------------------------------------------
// One metric family, rendered.
// ---------------------------------------------------------------------------

/// A metric family under construction: its identity, and the series pushed into it so far.
///
/// Series are kept in first-seen order so a scrape is byte-identical for identical state — a diff
/// of two scrapes then shows what changed in the install, not what a hash map's iteration decided.
pub(super) struct Family {
    name: &'static str,
    help: &'static str,
    kind: &'static str,
    label_names: &'static [&'static str],
    order: Vec<String>,
    values: BTreeMap<String, f64>,
    seen: BTreeSet<String>,
}

impl Family {
    pub(super) fn counter(name: &'static str, help: &'static str, label_names: &'static [&'static str]) -> Self {
        Family::of(name, help, "counter", label_names)
    }

    pub(super) fn gauge(name: &'static str, help: &'static str, label_names: &'static [&'static str]) -> Self {
        Family::of(name, help, "gauge", label_names)
    }

    pub(super) fn of(name: &'static str, help: &'static str, kind: &'static str, label_names: &'static [&'static str]) -> Self {
        Family {
            name,
            help,
            kind,
            label_names,
            order: Vec::new(),
            values: BTreeMap::new(),
            seen: BTreeSet::new(),
        }
    }

    /// Adds one observation. A label set already present adds to it; a new one is admitted until
    /// the cap, after which every further set lands in the single `other` series.
    pub(super) fn push(&mut self, label_values: Vec<String>, value: f64) {
        let key = self.admit(label_values);
        *self.values.entry(key).or_insert(0.0) += value;
    }

    /// The key an incoming label set is recorded under: itself while there is room, the folded
    /// `other` set once there is not.
    pub(super) fn admit(&mut self, label_values: Vec<String>) -> String {
        let key = label_values.join("\u{1f}");
        if self.seen.contains(&key) || self.seen.len() < SERIES_CAP {
            if self.seen.insert(key.clone()) {
                self.order.push(key.clone());
            }
            return key;
        }
        let folded = vec![OTHER.to_string(); self.label_names.len()].join("\u{1f}");
        if self.seen.insert(folded.clone()) {
            self.order.push(folded.clone());
        }
        folded
    }

    /// `# HELP`, `# TYPE` and the family's series, in that order.
    pub(super) fn render(&self, out: &mut String) {
        self.head(out);
        for key in &self.order {
            let Some(value) = self.values.get(key) else { continue };
            out.push_str(self.name);
            out.push_str(&label_block(self.label_names, &key.split('\u{1f}').collect::<Vec<_>>()));
            out.push(' ');
            out.push_str(&number(*value));
            out.push('\n');
        }
    }

    /// `# HELP` and `# TYPE` alone — what a family with no series yet still prints, so a dashboard
    /// that has never seen a sample still knows the metric exists.
    pub(super) fn head(&self, out: &mut String) {
        out.push_str("# HELP ");
        out.push_str(self.name);
        out.push(' ');
        out.push_str(self.help);
        out.push_str("\n# TYPE ");
        out.push_str(self.name);
        out.push(' ');
        out.push_str(self.kind);
        out.push('\n');
    }
}

/// `{a="1",b="2"}`, or nothing at all for a family with no labels.
pub(super) fn label_block(names: &[&str], values: &[&str]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = names
        .iter()
        .zip(values)
        .map(|(name, value)| format!("{name}=\"{}\"", escape(value)))
        .collect();
    format!("{{{}}}", pairs.join(","))
}

/// A metric value as the exposition writes it: an integer when it is one, shortest round-trip
/// otherwise, and the three spelled-out words Go's parser expects.
pub(super) fn number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { LE_INF.into() } else { "-Inf".into() };
    }
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// One histogram family: `# HELP`, `# TYPE`, then per provider the cumulative buckets, the sum in
/// seconds and the count.
pub(super) struct HistogramFamily {
    name: &'static str,
    help: &'static str,
    series: Vec<(String, HistogramSnapshot)>,
}

impl HistogramFamily {
    pub(super) fn new(name: &'static str, help: &'static str) -> Self {
        HistogramFamily {
            name,
            help,
            series: Vec::new(),
        }
    }

    /// Adds one provider's histogram, folding everything past [`SERIES_CAP`] providers into the
    /// `other` series by summing the counters — a histogram that adds up is still a histogram.
    pub(super) fn push(&mut self, provider: &str, snapshot: HistogramSnapshot) {
        if self.series.len() < SERIES_CAP && !self.series.iter().any(|(p, _)| p == provider) {
            self.series.push((provider.to_string(), snapshot));
            return;
        }
        if let Some((_, other)) = self.series.iter_mut().find(|(p, _)| p == OTHER) {
            *other = merge(*other, snapshot);
        } else {
            self.series.push((OTHER.to_string(), snapshot));
        }
    }

    /// The family as its own document, for a test that renders one thing on its own.
    #[cfg(test)]
    pub(super) fn render_into(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out
    }

    pub(super) fn render(&self, out: &mut String) {
        Family::of(self.name, self.help, "histogram", &["provider"]).head(out);
        for (provider, snapshot) in &self.series {
            let mut running = 0u64;
            for (at, label) in LE_LABELS.iter().enumerate() {
                running += snapshot.buckets[at];
                out.push_str(self.name);
                out.push_str("_bucket");
                out.push_str(&label_block(&["provider", "le"], &[provider.as_str(), label]));
                out.push(' ');
                out.push_str(&number(running as f64));
                out.push('\n');
            }
            out.push_str(self.name);
            out.push_str("_bucket");
            out.push_str(&label_block(&["provider", "le"], &[provider.as_str(), LE_INF]));
            out.push(' ');
            out.push_str(&number(snapshot.count as f64));
            out.push('\n');
            out.push_str(self.name);
            out.push_str("_sum");
            out.push_str(&label_block(&["provider"], &[provider.as_str()]));
            out.push(' ');
            out.push_str(&number(snapshot.sum_ms as f64 / 1000.0));
            out.push('\n');
            out.push_str(self.name);
            out.push_str("_count");
            out.push_str(&label_block(&["provider"], &[provider.as_str()]));
            out.push(' ');
            out.push_str(&number(snapshot.count as f64));
            out.push('\n');
        }
    }
}
