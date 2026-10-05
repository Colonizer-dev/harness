//! The export policy (docs/design/observability.md, P2–P10): the only path by which a string
//! reaches an OTLP record. A mapper never fills an OTLP message itself; it asks a [`Policy`] for a
//! builder — [`Policy::log`], [`Policy::span`], [`Policy::metric`] — and hands it values with a
//! [`Tier`]. The builder then, for every value:
//!
//! 1. drops a key that is not on that record kind's allowlist ([`allowlist`]); that is a bug in
//!    the mapper, so it also fails a `debug_assert!`;
//! 2. drops a content-tier value unless the [`ContentGate`] allows content, and always on a span
//!    (P5: content travels only as a log record's body or attribute);
//! 3. under `repo_names = hashed`, replaces a repository, org or path with its keyed hash and drops
//!    a branch, a URL naming the repository, or a title (P9);
//! 4. replaces image and base64 payloads with a placeholder (P8), redacts the string again (P7), and
//!    caps it — attributes at `max_attribute_bytes`, bodies at `max_content_bytes` — with the marker
//!    `…(N more bytes)`, setting `colonizer.truncated = true` on the record (P10).

pub mod allowlist;
mod scrub;

pub use allowlist::{Source, SpanKind, TRUNCATED};
pub use scrub::{placeholder, truncate};

use crate::batch::{ExportResource, Item, Record};
use crate::hashing::HashKey;
use crate::proto::common::v1::{AnyValue, ArrayValue, KeyValue, KeyValueList, any_value};
use crate::proto::logs::v1::{LogRecord, SeverityNumber};
use crate::proto::metrics::v1::{
    AggregationTemporality, Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint, Sum, metric, number_data_point,
};
use crate::proto::resource::v1::Resource;
use crate::proto::trace::v1::{Span, Status, span, status};
use allowlist::{Naming, Rule};
use colonizer_redact::{redact_text, redact_value};
use serde_json::Value;

/// How sensitive a value is (P2/P3). Content outranks structure: a value is content if either the
/// caller or the allowlist says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Ids, types, timings, names, sizes, counts, cost, status codes.
    Structure,
    /// Prompts, completions, tool arguments and results, question and answer text, file paths,
    /// command lines, error text, titles.
    Content,
}

/// Whether content-tier values may leave the machine (P3/P4). Until the gate's conditions (install
/// switch, org opt-in, known sensitivity) are wired in, it is always closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentGate {
    open: bool,
}

impl ContentGate {
    /// No content: the only gate outside tests.
    pub const fn closed() -> ContentGate {
        ContentGate { open: false }
    }

    /// Content allowed, for tests of what the rest of the policy does to it.
    #[cfg(test)]
    pub(crate) const fn open() -> ContentGate {
        ContentGate { open: true }
    }

    pub fn allows(self) -> bool {
        self.open
    }
}

/// The `repo_names` setting (P9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepoNames {
    #[default]
    Plain,
    Hashed,
}

/// The settings the policy reads (docs/observability/settings.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolicyConfig {
    pub max_attribute_bytes: usize,
    pub max_content_bytes: usize,
    pub repo_names: RepoNames,
}

impl PolicyConfig {
    pub const ATTRIBUTE_BYTES: std::ops::RangeInclusive<usize> = 128..=8192;
    pub const CONTENT_BYTES: std::ops::RangeInclusive<usize> = 1024..=196_608;

    /// The config with its caps held to the ranges settings validation allows, so a hand-edited
    /// file cannot lift them.
    pub fn clamped(self) -> PolicyConfig {
        let clamp = |v: usize, r: std::ops::RangeInclusive<usize>| v.clamp(*r.start(), *r.end());
        PolicyConfig {
            max_attribute_bytes: clamp(self.max_attribute_bytes, Self::ATTRIBUTE_BYTES),
            max_content_bytes: clamp(self.max_content_bytes, Self::CONTENT_BYTES),
            repo_names: self.repo_names,
        }
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        PolicyConfig {
            max_attribute_bytes: 1024,
            max_content_bytes: 32_768,
            repo_names: RepoNames::Plain,
        }
    }
}

/// An attribute's value before the policy has seen it.
#[derive(Clone, Debug, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
}

impl From<&str> for AttrValue {
    fn from(v: &str) -> Self {
        AttrValue::Str(v.to_string())
    }
}
impl From<String> for AttrValue {
    fn from(v: String) -> Self {
        AttrValue::Str(v)
    }
}
impl From<i64> for AttrValue {
    fn from(v: i64) -> Self {
        AttrValue::Int(v)
    }
}
impl From<f64> for AttrValue {
    fn from(v: f64) -> Self {
        AttrValue::Double(v)
    }
}
impl From<bool> for AttrValue {
    fn from(v: bool) -> Self {
        AttrValue::Bool(v)
    }
}

/// The export policy: config, content gate and (for `hashed`) the hash key.
#[derive(Clone, Debug)]
pub struct Policy {
    config: PolicyConfig,
    gate: ContentGate,
    key: Option<HashKey>,
}

impl Policy {
    /// `key` is needed for `repo_names = hashed`; without one, every value that would be hashed is
    /// dropped instead (fail closed), never sent in plain.
    pub fn new(config: PolicyConfig, gate: ContentGate, key: Option<HashKey>) -> Policy {
        Policy {
            config: config.clamped(),
            gate,
            key,
        }
    }

    pub fn config(&self) -> PolicyConfig {
        self.config
    }

    /// A log record from `source`.
    pub fn log(&self, source: Source) -> LogBuilder<'_> {
        LogBuilder {
            fields: Fields::new(self, allowlist::for_source(source), false),
            record: LogRecord::default(),
        }
    }

    /// A span of `kind`, named `<kind> <subject>` (`execute_tool Bash`, `invoke_agent acme/widgets`).
    /// An `invoke_agent` span's subject is its repository, hashed under `hashed`. Every other kind's
    /// subject must be structure — a tool, model or step name — and never a repository, org, branch
    /// or path, since `hashed` does not reach it. The name is still redacted and capped.
    pub fn span(&self, kind: SpanKind, subject: &str) -> SpanBuilder<'_> {
        let mut fields = Fields::new(self, allowlist::for_span(kind), true);
        let subject = match (kind, self.config.repo_names) {
            (SpanKind::InvokeAgent, RepoNames::Hashed) => self.hash(subject).unwrap_or_default(),
            _ => subject.to_string(),
        };
        let name = if subject.is_empty() {
            kind.as_str().to_string()
        } else {
            format!("{} {subject}", kind.as_str())
        };
        let name = fields.text(&name, None, self.config.max_attribute_bytes);
        SpanBuilder {
            fields,
            span: Span {
                name,
                kind: span::SpanKind::Internal as i32,
                ..Span::default()
            },
        }
    }

    /// A metric `name` (in `unit`) from `source`, with one data point. A gauge unless
    /// [`MetricBuilder::sum`] says otherwise.
    pub fn metric(&self, source: Source, name: &'static str, unit: &'static str) -> MetricBuilder<'_> {
        MetricBuilder {
            fields: Fields::new(self, allowlist::for_source(source), true),
            name,
            unit,
            sum: None,
            histogram: None,
            point: NumberDataPoint::default(),
        }
    }

    /// The resource every request carries (`service.name`, the install's ids). Any key is allowed —
    /// these are the add-on's own — but values are still redacted and capped.
    pub fn resource(&self, attributes: &[(&str, AttrValue)]) -> ExportResource {
        let attributes = attributes
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    AttrValue::Str(s) => AttrValue::Str(scrub_text(s, Some(key), self.config.max_attribute_bytes).0),
                    other => other.clone(),
                };
                kv(key, value)
            })
            .collect();
        ExportResource(Resource {
            attributes,
            ..Resource::default()
        })
    }

    fn hash(&self, name: &str) -> Option<String> {
        self.key.as_ref().map(|k| k.hash_name(name))
    }
}

/// A string through the export pipeline: payloads replaced (P8), redacted (P7), capped (P10).
/// Whether the cap cut it comes back alongside. With the attribute `key` it is held under, it is
/// redacted the way the same field is on disk: a secret-named key (`password`) loses its value
/// whatever it looks like, and an identifier or digest key (`tool_call_id`, `*.sha`) skips only the
/// high-entropy layer, so a long provider id stays joinable while a known token shape is still caught.
pub(crate) fn scrub_text(s: &str, key: Option<&str>, cap: usize) -> (String, bool) {
    let unblobbed = scrub::blobs(s);
    let redacted = match key {
        None => redact_text(&unblobbed).into_owned(),
        Some(key) => redact_under(key, unblobbed.into_owned()),
    };
    let cut = redacted.len() > cap;
    (truncate(&redacted, cap).into_owned(), cut)
}

/// `value` redacted as the field `key` of a JSON object: colonizer-redact's key-aware path.
fn redact_under(key: &str, value: String) -> String {
    let mut field = Value::Object(serde_json::Map::from_iter([(key.to_string(), Value::String(value))]));
    redact_value(&mut field);
    match field {
        Value::Object(mut map) => match map.remove(key) {
            Some(Value::String(s)) => s,
            // Not reachable: redaction keeps a field's key and its string-ness. Fail closed anyway.
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The attributes of one record under construction, and whether anything in it was cut.
struct Fields<'p> {
    policy: &'p Policy,
    tables: [&'static [Rule]; 2],
    span: bool,
    attributes: Vec<KeyValue>,
    truncated: bool,
}

impl<'p> Fields<'p> {
    fn new(policy: &'p Policy, tables: [&'static [Rule]; 2], span: bool) -> Self {
        Fields {
            policy,
            tables,
            span,
            attributes: Vec::new(),
            truncated: false,
        }
    }

    fn text(&mut self, s: &str, key: Option<&str>, cap: usize) -> String {
        let (s, cut) = scrub_text(s, key, cap);
        self.truncated |= cut;
        s
    }

    /// Steps 1–4 of the module doc for one attribute; a later value for the same key replaces it.
    fn attr(&mut self, key: &str, value: AttrValue, tier: Tier) {
        let Some(rule) = allowlist::lookup(&self.tables, key) else {
            debug_assert!(false, "attribute key {key:?} is not on this record's allowlist");
            return;
        };
        // A key under a prefix (`model_usage.<model>`) carries data in its name, and keys are sent as
        // they are: one the redactor would change is dropped with its value, never sent cleaned.
        if rule.key.ends_with('*') && matches!(redact_text(key), std::borrow::Cow::Owned(_)) {
            return;
        }
        if tier.max(rule.tier) == Tier::Content && (self.span || !self.policy.gate.allows()) {
            return;
        }
        let policy = self.policy;
        let value = match (rule.naming, policy.config.repo_names) {
            (Naming::Dropped, RepoNames::Hashed) => return,
            (Naming::Hashed, RepoNames::Hashed) => {
                let raw = match &value {
                    AttrValue::Str(s) => s.clone(),
                    AttrValue::Int(i) => i.to_string(),
                    AttrValue::Double(d) => d.to_string(),
                    AttrValue::Bool(b) => b.to_string(),
                };
                match policy.hash(&raw) {
                    Some(hash) => AttrValue::Str(hash),
                    None => return,
                }
            }
            _ => match value {
                AttrValue::Str(s) => AttrValue::Str(self.text(&s, Some(key), policy.config.max_attribute_bytes)),
                other => other,
            },
        };
        self.attributes.retain(|kv| kv.key != key);
        self.attributes.push(kv(key, value));
    }

    /// The attributes, with `colonizer.truncated` added when anything was cut.
    fn finish(mut self) -> Vec<KeyValue> {
        if self.truncated {
            self.attributes.retain(|kv| kv.key != TRUNCATED);
            self.attributes.push(kv(TRUNCATED, AttrValue::Bool(true)));
        }
        self.attributes
    }
}

fn kv(key: &str, value: AttrValue) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(any(value)),
        ..KeyValue::default()
    }
}

fn any(value: AttrValue) -> AnyValue {
    let value = match value {
        AttrValue::Str(s) => any_value::Value::StringValue(s),
        AttrValue::Int(i) => any_value::Value::IntValue(i),
        // OTLP/JSON has no number for NaN or infinity; send its name rather than a broken document.
        AttrValue::Double(d) if !d.is_finite() => any_value::Value::StringValue(d.to_string()),
        AttrValue::Double(d) => any_value::Value::DoubleValue(d),
        AttrValue::Bool(b) => any_value::Value::BoolValue(b),
    };
    AnyValue { value: Some(value) }
}

/// An already scrubbed JSON value as an OTLP value; object keys are redacted too.
fn json_any(value: Value) -> AnyValue {
    let value = match value {
        Value::Null => return AnyValue { value: None },
        Value::Bool(b) => any_value::Value::BoolValue(b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => any_value::Value::IntValue(i),
            None => any_value::Value::DoubleValue(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => any_value::Value::StringValue(s),
        Value::Array(items) => any_value::Value::ArrayValue(ArrayValue {
            values: items.into_iter().map(json_any).collect(),
        }),
        Value::Object(map) => any_value::Value::KvlistValue(KeyValueList {
            values: map
                .into_iter()
                .map(|(k, v)| KeyValue {
                    key: redact_text(&k).into_owned(),
                    value: Some(json_any(v)),
                    ..KeyValue::default()
                })
                .collect(),
        }),
    };
    AnyValue { value: Some(value) }
}

/// A log record under construction; see [`Policy::log`].
pub struct LogBuilder<'p> {
    fields: Fields<'p>,
    record: LogRecord,
}

impl LogBuilder<'_> {
    /// When it happened, which is also when it was observed, in Unix nanoseconds.
    pub fn time(mut self, unix_nanos: u64) -> Self {
        self.record.time_unix_nano = unix_nanos;
        self.record.observed_time_unix_nano = unix_nanos;
        self
    }

    pub fn severity(mut self, severity: SeverityNumber) -> Self {
        let n = severity as i32;
        self.record.severity_number = n;
        self.record.severity_text = match n {
            1..=4 => "TRACE",
            5..=8 => "DEBUG",
            9..=12 => "INFO",
            13..=16 => "WARN",
            17..=20 => "ERROR",
            21..=24 => "FATAL",
            _ => "",
        }
        .to_string();
        self
    }

    /// The trace and span the record belongs to.
    pub fn trace(mut self, trace_id: [u8; 16], span_id: [u8; 8]) -> Self {
        self.record.trace_id = trace_id.to_vec();
        self.record.span_id = span_id.to_vec();
        self
    }

    /// The event's name (`claude_code.api_request`). Structure only: a name from a fixed set, never one
    /// built from a repository, org, branch or path, since `repo_names = hashed` does not reach it. It
    /// is still redacted and capped.
    pub fn event_name(mut self, name: &str) -> Self {
        self.record.event_name = self.fields.text(name, None, self.fields.policy.config.max_attribute_bytes);
        self
    }

    pub fn attr(mut self, key: &str, value: impl Into<AttrValue>, tier: Tier) -> Self {
        self.fields.attr(key, value.into(), tier);
        self
    }

    /// The record's body as text; content tier needs the gate.
    pub fn body(mut self, text: &str, tier: Tier) -> Self {
        if self.allowed(tier) {
            let text = self.fields.text(text, None, self.fields.policy.config.max_content_bytes);
            self.record.body = Some(any(AttrValue::Str(text)));
        }
        self
    }

    /// The record's body as structured JSON, redacted field by field. One that would not fit
    /// `max_content_bytes` is sent as its JSON text, truncated.
    pub fn body_json(mut self, value: &Value, tier: Tier) -> Self {
        if !self.allowed(tier) {
            return self;
        }
        let mut value = value.clone();
        scrub::json_blobs(&mut value);
        redact_value(&mut value);
        let cap = self.fields.policy.config.max_content_bytes;
        let text = value.to_string();
        self.record.body = Some(if text.len() > cap {
            // Already payload-free and redacted; this pass is the cap (and redacts again, harmlessly).
            any(AttrValue::Str(self.fields.text(&text, None, cap)))
        } else {
            json_any(value)
        });
        self
    }

    fn allowed(&self, tier: Tier) -> bool {
        tier == Tier::Structure || self.fields.policy.gate.allows()
    }

    pub fn finish(mut self) -> Item {
        self.record.attributes = self.fields.finish();
        Item(Record::Log(self.record))
    }
}

/// A span under construction; see [`Policy::span`]. It carries structure only (P5).
pub struct SpanBuilder<'p> {
    fields: Fields<'p>,
    span: Span,
}

impl SpanBuilder<'_> {
    pub fn ids(mut self, trace_id: [u8; 16], span_id: [u8; 8], parent_span_id: Option<[u8; 8]>) -> Self {
        self.span.trace_id = trace_id.to_vec();
        self.span.span_id = span_id.to_vec();
        self.span.parent_span_id = parent_span_id.map(|p| p.to_vec()).unwrap_or_default();
        self
    }

    pub fn times(mut self, start_unix_nanos: u64, end_unix_nanos: u64) -> Self {
        self.span.start_time_unix_nano = start_unix_nanos;
        self.span.end_time_unix_nano = end_unix_nanos;
        self
    }

    /// The status code. A status message is error text — content — so a span never has one.
    pub fn status(mut self, code: status::StatusCode) -> Self {
        self.span.status = Some(Status {
            code: code as i32,
            ..Status::default()
        });
        self
    }

    pub fn attr(mut self, key: &str, value: impl Into<AttrValue>, tier: Tier) -> Self {
        self.fields.attr(key, value.into(), tier);
        self
    }

    /// The span's kind; [`span::SpanKind::Internal`] unless set.
    pub fn kind(mut self, kind: span::SpanKind) -> Self {
        self.span.kind = kind as i32;
        self
    }

    /// A span event: a name from a fixed set (`colonizer.suspended`) at a time, with no attributes.
    /// Like a span's name it is structure, so it is still redacted and capped.
    pub fn event(mut self, name: &str, unix_nanos: u64) -> Self {
        let name = self.fields.text(name, None, self.fields.policy.config.max_attribute_bytes);
        self.span.events.push(span::Event {
            time_unix_nano: unix_nanos,
            name,
            ..span::Event::default()
        });
        self
    }

    pub fn finish(mut self) -> Item {
        self.span.attributes = self.fields.finish();
        Item(Record::Span(self.span))
    }
}

/// A metric under construction; see [`Policy::metric`].
pub struct MetricBuilder<'p> {
    fields: Fields<'p>,
    name: &'static str,
    unit: &'static str,
    sum: Option<bool>,
    histogram: Option<HistogramDataPoint>,
    point: NumberDataPoint,
}

impl MetricBuilder<'_> {
    /// A cumulative sum rather than a gauge; `monotonic` for a counter.
    pub fn sum(mut self, monotonic: bool) -> Self {
        self.sum = Some(monotonic);
        self
    }

    pub fn times(mut self, start_unix_nanos: u64, unix_nanos: u64) -> Self {
        self.point.start_time_unix_nano = start_unix_nanos;
        self.point.time_unix_nano = unix_nanos;
        self
    }

    pub fn int(mut self, value: i64) -> Self {
        self.point.value = Some(number_data_point::Value::AsInt(value));
        self
    }

    /// A non-finite value is left out: OTLP/JSON could not carry it.
    pub fn double(mut self, value: f64) -> Self {
        self.point.value = value.is_finite().then_some(number_data_point::Value::AsDouble(value));
        self
    }

    /// A cumulative explicit-bucket histogram instead of a number: `counts` has one more entry than
    /// `bounds` (the last bucket is everything above the last bound), and `sum` is the sum of every
    /// observation. A non-finite `sum` is left out, as [`MetricBuilder::double`] leaves one out.
    pub fn histogram(mut self, bounds: &[f64], counts: &[u64], sum: f64) -> Self {
        self.histogram = Some(HistogramDataPoint {
            count: counts.iter().sum(),
            sum: sum.is_finite().then_some(sum),
            bucket_counts: counts.to_vec(),
            explicit_bounds: bounds.to_vec(),
            ..HistogramDataPoint::default()
        });
        self
    }

    /// A point attribute: structure only, like a span's.
    pub fn attr(mut self, key: &str, value: impl Into<AttrValue>, tier: Tier) -> Self {
        self.fields.attr(key, value.into(), tier);
        self
    }

    pub fn finish(mut self) -> Item {
        let attributes = self.fields.finish();
        if let Some(mut point) = self.histogram {
            point.attributes = attributes;
            point.start_time_unix_nano = self.point.start_time_unix_nano;
            point.time_unix_nano = self.point.time_unix_nano;
            return Item(Record::Metric(Metric {
                name: self.name.to_string(),
                unit: self.unit.to_string(),
                data: Some(metric::Data::Histogram(Histogram {
                    data_points: vec![point],
                    aggregation_temporality: AggregationTemporality::Cumulative as i32,
                })),
                ..Metric::default()
            }));
        }
        self.point.attributes = attributes;
        let data_points = vec![self.point];
        let data = match self.sum {
            None => metric::Data::Gauge(Gauge { data_points }),
            Some(is_monotonic) => metric::Data::Sum(Sum {
                data_points,
                aggregation_temporality: AggregationTemporality::Cumulative as i32,
                is_monotonic,
            }),
        };
        Item(Record::Metric(Metric {
            name: self.name.to_string(),
            unit: self.unit.to_string(),
            data: Some(data),
            ..Metric::default()
        }))
    }
}

#[cfg(test)]
mod tests;
