//! Packing records into OTLP export requests that never exceed a byte budget.
//!
//! A [`Batcher`] takes the policy's [`Item`]s and returns requests, one signal each, every one
//! carrying the install's single [`ExportResource`] and the scope `colonizer` at this crate's
//! version. A request holds at most `max_items` records and encodes, in the batch's [`Encoding`],
//! to at most `max_request_bytes` — sized exactly as items are added, then checked once more on the
//! built request and split further if it is somehow over.
//!
//! A record too big for a request on its own is cut first (its longest strings truncated with the
//! policy's marker, `colonizer.truncated = true` added); one that still cannot fit is dropped and
//! counted in [`Batch::oversized`], never sent whole and never split across requests.

use crate::encode::{Encoding, Request, fix_json, json_bytes};
use crate::policy::{TRUNCATED, truncate};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::proto::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use crate::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use crate::proto::metrics::v1::{Metric, ResourceMetrics, ScopeMetrics, metric};
use crate::proto::resource::v1::Resource;
use crate::proto::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;

/// The default request budget: 1 MiB.
pub const DEFAULT_REQUEST_BYTES: usize = 1_048_576;
/// No request is ever bigger than this, whatever the config says: 4 MiB, the common collector
/// receive limit.
pub const MAX_REQUEST_BYTES: usize = 4_194_304;
/// The smallest budget a config can ask for.
pub const MIN_REQUEST_BYTES: usize = 1024;
/// The default records per request.
pub const DEFAULT_ITEMS: usize = 2000;

/// A record the policy built: the only thing a [`Batcher`] accepts.
#[derive(Clone, Debug, PartialEq)]
pub struct Item(pub(crate) Record);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Record {
    Log(LogRecord),
    Span(Span),
    Metric(Metric),
}

/// The resource every request carries, built by [`crate::policy::Policy::resource`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportResource(pub(crate) Resource);

/// How requests are bounded and encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchConfig {
    pub max_request_bytes: usize,
    pub max_items: usize,
    pub encoding: Encoding,
}

impl Default for BatchConfig {
    fn default() -> Self {
        BatchConfig {
            max_request_bytes: DEFAULT_REQUEST_BYTES,
            max_items: DEFAULT_ITEMS,
            encoding: Encoding::Protobuf,
        }
    }
}

impl BatchConfig {
    /// The config held to [`MIN_REQUEST_BYTES`]`..=`[`MAX_REQUEST_BYTES`] and at least one item.
    pub fn clamped(self) -> BatchConfig {
        BatchConfig {
            max_request_bytes: self.max_request_bytes.clamp(MIN_REQUEST_BYTES, MAX_REQUEST_BYTES),
            max_items: self.max_items.max(1),
            encoding: self.encoding,
        }
    }
}

/// What a [`Batcher`] produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub requests: Vec<Request>,
    /// Records dropped because even truncated they would not fit a request.
    pub oversized: usize,
    /// Records truncated to fit a request.
    pub truncated: usize,
}

/// The protobuf envelope's two nested lengths (resource-, then scope-level) grow as items are
/// added; a varint grows by at most 3 bytes between an empty message and a 4 MiB one.
const PROTOBUF_SLACK: usize = 8;
/// A string is never cut shorter than this to make a record fit; at that point it is dropped.
const MIN_STRING_BYTES: usize = 64;
/// Passes at shrinking one record before giving up on it.
const SHRINK_PASSES: usize = 64;

/// Packs [`Item`]s into requests; see the module doc.
#[derive(Debug)]
pub struct Batcher {
    resource: Resource,
    scope: InstrumentationScope,
    config: BatchConfig,
    logs: Lane<LogRecord>,
    spans: Lane<Span>,
    metrics: Lane<Metric>,
    batch: Batch,
}

impl Batcher {
    pub fn new(resource: &ExportResource, config: BatchConfig) -> Batcher {
        let resource = resource.0.clone();
        let scope = InstrumentationScope {
            name: "colonizer".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..InstrumentationScope::default()
        };
        let config = config.clamped();
        Batcher {
            logs: Lane::new(&resource, &scope, config.encoding),
            spans: Lane::new(&resource, &scope, config.encoding),
            metrics: Lane::new(&resource, &scope, config.encoding),
            resource,
            scope,
            config,
            batch: Batch::default(),
        }
    }

    pub fn push(&mut self, item: Item) {
        let ctx = Ctx {
            resource: &self.resource,
            scope: &self.scope,
            config: self.config,
            batch: &mut self.batch,
        };
        match item.0 {
            Record::Log(r) => self.logs.push(r, ctx),
            Record::Span(s) => self.spans.push(s, ctx),
            Record::Metric(m) => self.metrics.push(m, ctx),
        }
    }

    /// Every request, the partly filled ones included.
    pub fn finish(mut self) -> Batch {
        let mut ctx = Ctx {
            resource: &self.resource,
            scope: &self.scope,
            config: self.config,
            batch: &mut self.batch,
        };
        self.logs.flush(&mut ctx);
        self.spans.flush(&mut ctx);
        self.metrics.flush(&mut ctx);
        self.batch
    }
}

/// The batcher's shared state, lent to one lane at a time.
struct Ctx<'a> {
    resource: &'a Resource,
    scope: &'a InstrumentationScope,
    config: BatchConfig,
    batch: &'a mut Batch,
}

/// One signal's record type: how it wraps into a request and where its strings are.
trait Signal: Message + Clone + Default + Sized {
    fn request(resource: Resource, scope: InstrumentationScope, items: Vec<Self>) -> Request;
    fn json_len(&self) -> usize;
    fn attributes(&mut self) -> Option<&mut Vec<KeyValue>>;
    fn strings(&mut self) -> Vec<&mut String>;
    fn take(request: Request) -> Vec<Self>;
}

/// The records of one signal waiting for their request, and what that request would weigh.
#[derive(Debug)]
struct Lane<T> {
    items: Vec<T>,
    /// The bytes the items add to the envelope (JSON separators included).
    bytes: usize,
    /// The request with no items, encoded (protobuf: plus [`PROTOBUF_SLACK`]).
    envelope: usize,
}

impl<T: Signal> Lane<T> {
    fn new(resource: &Resource, scope: &InstrumentationScope, encoding: Encoding) -> Self {
        let empty = T::request(resource.clone(), scope.clone(), Vec::new());
        let envelope = match encoding {
            Encoding::Protobuf => empty.encoded_len(encoding) + PROTOBUF_SLACK,
            Encoding::Json => empty.encoded_len(encoding),
        };
        Lane {
            items: Vec::new(),
            bytes: 0,
            envelope,
        }
    }

    /// What `item` adds to a request: its field (tag, length, body) in protobuf; its text in JSON,
    /// plus the comma before it unless it is first.
    fn part(item: &T, encoding: Encoding, first: bool) -> usize {
        match encoding {
            Encoding::Protobuf => {
                let len = item.encoded_len();
                1 + prost::encoding::encoded_len_varint(len as u64) + len
            }
            Encoding::Json => item.json_len() + usize::from(!first),
        }
    }

    fn push(&mut self, mut item: T, mut ctx: Ctx<'_>) {
        let encoding = ctx.config.encoding;
        let room = ctx.config.max_request_bytes.saturating_sub(self.envelope);
        if Self::part(&item, encoding, true) > room {
            if !shrink(&mut item, room, encoding) {
                ctx.batch.oversized += 1;
                return;
            }
            ctx.batch.truncated += 1;
        }
        let mut part = Self::part(&item, encoding, self.items.is_empty());
        if self.items.len() >= ctx.config.max_items || self.bytes + part > room {
            self.flush(&mut ctx);
            part = Self::part(&item, encoding, true);
        }
        self.items.push(item);
        self.bytes += part;
    }

    fn flush(&mut self, ctx: &mut Ctx<'_>) {
        self.bytes = 0;
        if self.items.is_empty() {
            return;
        }
        let items = std::mem::take(&mut self.items);
        emit(T::request(ctx.resource.clone(), ctx.scope.clone(), items), ctx);
    }
}

/// `request` into the batch, checked against the budget one last time: one over it (the sizing
/// above is exact, so this is a guard, not a path) is halved until each half fits.
fn emit(request: Request, ctx: &mut Ctx<'_>) {
    if request.encoded_len(ctx.config.encoding) <= ctx.config.max_request_bytes {
        ctx.batch.requests.push(request);
        return;
    }
    match request {
        Request::Logs(_) => halve::<LogRecord>(request, ctx),
        Request::Traces(_) => halve::<Span>(request, ctx),
        Request::Metrics(_) => halve::<Metric>(request, ctx),
    }
}

fn halve<T: Signal>(request: Request, ctx: &mut Ctx<'_>) {
    let mut items = T::take(request);
    if items.len() <= 1 {
        ctx.batch.oversized += items.len();
        return;
    }
    let back = items.split_off(items.len() / 2);
    for half in [items, back] {
        emit(T::request(ctx.resource.clone(), ctx.scope.clone(), half), ctx);
    }
}

/// Cuts `item`'s longest strings until it fits `room` bytes (as a request's only item); `false`
/// when it cannot without cutting a string below [`MIN_STRING_BYTES`].
fn shrink<T: Signal>(item: &mut T, room: usize, encoding: Encoding) -> bool {
    if let Some(attributes) = item.attributes()
        && !attributes.iter().any(|kv| kv.key == TRUNCATED)
    {
        attributes.push(KeyValue {
            key: TRUNCATED.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::BoolValue(true)),
            }),
            ..KeyValue::default()
        });
    }
    for _ in 0..SHRINK_PASSES {
        let size = Lane::<T>::part(item, encoding, true);
        if size <= room {
            return true;
        }
        let excess = size - room;
        let mut strings = item.strings();
        let Some(longest) = strings.iter_mut().max_by_key(|s| s.len()) else {
            return false;
        };
        if longest.len() <= MIN_STRING_BYTES {
            return false;
        }
        // The excess in the string's own bytes (JSON escaping makes it weigh more encoded than it
        // is long), and a little more for the marker.
        let weight = match encoding {
            Encoding::Protobuf => longest.len(),
            Encoding::Json => serde_json::to_string(longest.as_str()).map_or(longest.len(), |j| j.len() - 2),
        };
        let cut = (excess * longest.len()).div_ceil(weight.max(1)) + 32;
        let cap = longest.len().saturating_sub(cut).max(MIN_STRING_BYTES);
        let cut = truncate(longest, cap).into_owned();
        **longest = cut;
    }
    false
}

/// Every string value under `value`, for [`shrink`].
fn any_strings<'a>(value: &'a mut AnyValue, out: &mut Vec<&'a mut String>) {
    match value.value.as_mut() {
        Some(any_value::Value::StringValue(s)) => out.push(s),
        Some(any_value::Value::ArrayValue(a)) => a.values.iter_mut().for_each(|v| any_strings(v, out)),
        Some(any_value::Value::KvlistValue(l)) => kv_strings(&mut l.values, out),
        _ => {}
    }
}

fn kv_strings<'a>(attributes: &'a mut [KeyValue], out: &mut Vec<&'a mut String>) {
    for v in attributes.iter_mut().filter_map(|kv| kv.value.as_mut()) {
        any_strings(v, out);
    }
}

/// One record's OTLP/JSON length, as it appears inside a request.
fn json_len(value: serde_json::Result<serde_json::Value>) -> usize {
    json_bytes(fix_json(value.expect("OTLP messages serialize to JSON"))).len()
}

impl Signal for LogRecord {
    fn request(resource: Resource, scope: InstrumentationScope, log_records: Vec<Self>) -> Request {
        Request::Logs(ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(resource),
                scope_logs: vec![ScopeLogs {
                    scope: Some(scope),
                    log_records,
                    ..ScopeLogs::default()
                }],
                ..ResourceLogs::default()
            }],
        })
    }

    fn json_len(&self) -> usize {
        json_len(serde_json::to_value(self))
    }

    fn attributes(&mut self) -> Option<&mut Vec<KeyValue>> {
        Some(&mut self.attributes)
    }

    fn strings(&mut self) -> Vec<&mut String> {
        let mut out = vec![&mut self.event_name];
        if let Some(body) = self.body.as_mut() {
            any_strings(body, &mut out);
        }
        kv_strings(&mut self.attributes, &mut out);
        out
    }

    fn take(request: Request) -> Vec<Self> {
        match request {
            Request::Logs(r) => r
                .resource_logs
                .into_iter()
                .flat_map(|r| r.scope_logs)
                .flat_map(|s| s.log_records)
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl Signal for Span {
    fn request(resource: Resource, scope: InstrumentationScope, spans: Vec<Self>) -> Request {
        Request::Traces(ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(resource),
                scope_spans: vec![ScopeSpans {
                    scope: Some(scope),
                    spans,
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        })
    }

    fn json_len(&self) -> usize {
        json_len(serde_json::to_value(self))
    }

    fn attributes(&mut self) -> Option<&mut Vec<KeyValue>> {
        Some(&mut self.attributes)
    }

    fn strings(&mut self) -> Vec<&mut String> {
        let mut out = vec![&mut self.name];
        kv_strings(&mut self.attributes, &mut out);
        out
    }

    fn take(request: Request) -> Vec<Self> {
        match request {
            Request::Traces(r) => r
                .resource_spans
                .into_iter()
                .flat_map(|r| r.scope_spans)
                .flat_map(|s| s.spans)
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl Signal for Metric {
    fn request(resource: Resource, scope: InstrumentationScope, metrics: Vec<Self>) -> Request {
        Request::Metrics(ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(resource),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(scope),
                    metrics,
                    ..ScopeMetrics::default()
                }],
                ..ResourceMetrics::default()
            }],
        })
    }

    fn json_len(&self) -> usize {
        json_len(serde_json::to_value(self))
    }

    /// The first data point's: the policy builds metrics with exactly one.
    fn attributes(&mut self) -> Option<&mut Vec<KeyValue>> {
        match self.data.as_mut()? {
            metric::Data::Gauge(g) => g.data_points.first_mut().map(|p| &mut p.attributes),
            metric::Data::Sum(s) => s.data_points.first_mut().map(|p| &mut p.attributes),
            _ => None,
        }
    }

    fn strings(&mut self) -> Vec<&mut String> {
        let mut out = Vec::new();
        if let Some(attributes) = self.attributes() {
            kv_strings(attributes, &mut out);
        }
        out
    }

    fn take(request: Request) -> Vec<Self> {
        match request {
            Request::Metrics(r) => r
                .resource_metrics
                .into_iter()
                .flat_map(|r| r.scope_metrics)
                .flat_map(|s| s.metrics)
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
