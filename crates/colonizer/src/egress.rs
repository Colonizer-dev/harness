//! Egress policy (#303): what a colony's microVM may reach, on top of msb's fence.
//!
//! Two classes of traffic share one VM-level fence, enforced host-side by msb's userspace network
//! stack: the agent's model traffic (the provider gateway port, TLS-edge secret hosts) and
//! everything auxiliary a colony runs — package managers, curls, the agent's own GitHub calls.
//! The agent and every tool in the guest run as root, so there is no in-guest split that could
//! scope the fence to the first class: a policy here applies to the whole VM. `Open` keeps
//! today's `public` profile; `Allowlist` drops the public allow and keeps only what the harness
//! and the operator named. In both modes a fixed deny set — network-internal destinations, cloud
//! metadata, and the classifier gaps listed in docs/sandbox-network.md — is compiled ahead of any
//! configured rule, and msb's first-match-wins evaluation (`evaluate_egress`, network/lib/policy/
//! types.rs at fa3e439) makes that set impossible to reopen by configuration.
//!
//! Rules reach msb as `--net-rule` tokens, grammar
//! `<action>[:<direction>]@<target>[:<proto>[:<ports>]]` (crates/cli/lib/net_rule.rs at
//! fa3e439). Every target this module emits is checked against that parser: group keywords,
//! IPv4 CIDRs bare, IPv6 CIDRs bracketed.

use crate::{
    ApiResult, Shared, client_error,
    config::{ModulesConfig, setting_str},
    modules::schema_for,
    orgs::OrgSettings,
};
use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, Ipv6Addr};

/// How much egress a colony gets. `Open` is the default so an install that never hears of the
/// setting boots exactly as before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EgressMode {
    /// The `public` profile: any non-private destination, with the deny set below on top.
    #[default]
    Open,
    /// No profile allow at all: only the harness's own ports, the operator's allow list, and DNS.
    Allowlist,
}

impl EgressMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "open" => Ok(EgressMode::Open),
            "allowlist" => Ok(EgressMode::Allowlist),
            other => Err(format!("egress mode must be `open` or `allowlist`, not `{other}`")),
        }
    }
}

/// The policy one colony answers to, after the global/org merge: the mode plus the operator's
/// entries, each as the operator wrote it (validated; see [`parse_entry`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EgressPolicy {
    pub mode: EgressMode,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub block: Vec<String>,
}

/// Where each part of a resolved policy came from, for the record and the fleet view: `"global"`
/// or `"org"`, the lists naming every level that contributed entries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sources {
    pub mode: String,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub block: Vec<String>,
}

/// The resolved policy plus where it came from — what [`resolve`] hands a boot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    pub policy: EgressPolicy,
    pub sources: Sources,
}

/// Destinations every colony is denied, whatever the mode and whoever configured what. Emitted as
/// `deny@…` rules ahead of every configured rule, so first-match-wins holds the fence: only the
/// harness's own port-scoped allows (and DNS) precede them. Group keywords expand host-side in
/// msb's classifier (network/lib/policy/destination.rs at fa3e439); the explicit CIDRs close the
/// gaps where that classifier falls through to `Public` (docs/sandbox-network.md, "Open risks").
pub(crate) const ALWAYS_BLOCKED: &[&str] = &[
    // Groups. `private` covers RFC1918, CGNAT 100.64.0.0/10 and ULA fc00::/7 today
    // (`is_private`); `meta` is 169.254.169.254; `host` is the sandbox's own gateway IPs —
    // unreachable today except through the port-scoped allows above, and kept that way.
    "meta",
    "private",
    "loopback",
    "link-local",
    "multicast",
    "host",
    // Explicit. `169.254.169.254` restates `meta` so the record reads whole; `100.64.0.0/10` and
    // `[fc00::/7]` restate parts of `private` so a classifier change cannot quietly reopen them.
    "169.254.169.254",
    "0.0.0.0/8",
    "100.64.0.0/10",
    "192.0.0.0/24",
    "198.18.0.0/15",
    "240.0.0.0/4",
    "[fc00::/7]",
    "[fec0::/10]",
    "[64:ff9b::/96]",
    "[64:ff9b:1::/48]",
    "[2002::/16]",
];

/// The DNS allow, first so the `deny@host` behind it cannot take the gateway's port-53 forwarder
/// down with everything else: msb evaluates a DNS query against rules naming the Host group or
/// `Any` (`evaluate_dns_query`), so a bare `deny@host` would otherwise answer every lookup.
pub(crate) const DNS_ALLOW: &str = "allow@dns";

/// One validated entry, split the way the msb token needs it. IPv6 targets keep their brackets in
/// the token: unbracketed colons collide with the grammar's field separator, and `net_rule.rs`
/// routes only `[...]` targets to the IPv6 parser.
struct Entry {
    host: String,
    port: Option<u16>,
    /// `*.example.com` became a `suffix=` target.
    wildcard: bool,
}

/// Validates one `host[:port]` entry and answers its pieces. A host is an FQDN (at least two
/// labels, lowercase — msb canonicalises and would refuse anything else), an IPv4, a bracketed
/// IPv6, or a CIDR of either (`[...]` for IPv6). A port is 1-65535; no port or `:0` means every
/// port. Refused with a named reason: anything empty, anything carrying `@`, a comma or
/// whitespace, wildcards other than a leading `*.`, and single-label names (which in msb's
/// grammar are group keywords like `host` or `public`, not hostnames).
fn parse_entry(raw: &str) -> Result<Entry, String> {
    let refuse = |what: &str| format!("`{raw}` is not a host or host:port ({what})");
    if raw.is_empty() {
        return Err("an egress entry cannot be empty".into());
    }
    if raw.contains('@') {
        return Err(refuse("it carries an `@`"));
    }
    if raw.chars().any(char::is_whitespace) {
        return Err(refuse("it carries whitespace"));
    }
    if raw.contains(',') {
        return Err(refuse("entries are separated by commas; an entry cannot carry one"));
    }
    // Bracketed IPv6 address or CIDR; a port may follow the `]`.
    if let Some(inner) = raw.strip_prefix('[') {
        let (addr, rest) = inner.split_once(']').ok_or_else(|| refuse("an `[` without a `]`"))?;
        let prefix = match rest.strip_prefix(':') {
            // `:0` reads as every port, like no port at all.
            Some(port) if parse_port(port).is_ok_and(|p| p == 0) => None,
            Some(port) => Some(parse_port(port).map_err(|why| refuse(&why))?),
            None if rest.is_empty() => None,
            None => return Err(refuse("only `:port` may follow `]`")),
        };
        let (ip, bits) = addr
            .split_once('/')
            .map(|(ip, bits)| (ip, Some(bits)))
            .unwrap_or((addr, None));
        ip.parse::<Ipv6Addr>()
            .map_err(|_| refuse("the bracketed address is not an IPv6 address"))?;
        let bits = match bits {
            Some(bits) => bits
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= 128)
                .ok_or_else(|| refuse("an IPv6 prefix must be 0-128"))?,
            None => 128,
        };
        return Ok(Entry {
            host: format!("[{ip}/{bits}]"),
            port: prefix,
            wildcard: false,
        });
    }
    if raw.contains('[') || raw.contains(']') {
        return Err(refuse("brackets are only for IPv6 addresses, at the start"));
    }
    // IPv4 CIDR: the only other form carrying a `/`. An IPv6 CIDR must be bracketed.
    if let Some((ip, bits)) = raw.split_once('/') {
        let ip = ip
            .parse::<Ipv4Addr>()
            .map_err(|_| refuse("a bare CIDR must be IPv4; bracket IPv6 ones like [fc00::/7]"))?;
        let bits = bits
            .parse::<u8>()
            .ok()
            .filter(|p| *p <= 32)
            .ok_or_else(|| refuse("an IPv4 prefix must be 0-32"))?;
        return Ok(Entry {
            host: format!("{ip}/{bits}"),
            port: None,
            wildcard: false,
        });
    }
    // Host, with an optional `:port`. A bare hostname cannot carry a colon, so the port, if any,
    // is everything after the last one.
    let (host, port) = match raw.rsplit_once(':') {
        Some((host, port)) => {
            let port = parse_port(port).map_err(|why| refuse(&why))?;
            (host, if port == 0 { None } else { Some(port) })
        }
        None => (raw, None),
    };
    let (wildcard, name) = match host.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, host),
    };
    if name == "*" || name.is_empty() {
        return Err(refuse("a wildcard is only `*.` before a name"));
    }
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 {
        return Err(refuse(
            "a single-label name is refused: in msb's rule grammar those words are destination groups",
        ));
    }
    for label in &labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return Err(refuse(
                "labels are lowercase letters, digits and dashes, 1-63 of each, without leading or trailing dashes",
            ));
        }
    }
    if name.len() > 253 {
        return Err(refuse("the name is longer than 253 characters"));
    }
    if wildcard && name.parse::<Ipv4Addr>().is_ok() {
        return Err(refuse("a wildcard cannot precede an IP address"));
    }
    Ok(Entry {
        host: host.to_string(),
        port,
        wildcard,
    })
}

fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>().map_err(|_| format!("a port must be 0-65535, not `{raw}`"))
}

/// The msb rule target an entry compiles to: a bare target for all-ports entries, and
/// `<target>:tcp:<port>` for port-scoped ones. TCP is the only protocol a port names: the entries
/// exist for TLS hosts and web endpoints, and msb's grammar takes one protocol per rule.
pub(crate) fn entry_target(entry: &str) -> String {
    match parse_entry(entry) {
        Ok(parsed) => {
            let host = if parsed.wildcard {
                format!("suffix={}", parsed.host.trim_start_matches("*."))
            } else {
                parsed.host
            };
            match parsed.port {
                Some(port) => format!("{host}:tcp:{port}"),
                None => host,
            }
        }
        // Callers validate before they compile; a stored-but-invalid entry (a hand-edited file) is
        // dropped by `parse_entries`, so this arm exists to keep `entry_target` total.
        Err(_) => String::new(),
    }
}

/// Validates one `host[:port]` entry, with the parser's own named message. What an org save calls
/// per list item, where there is no comma separation to hide behind.
pub fn validate_entry(entry: &str) -> Result<(), String> {
    parse_entry(entry).map(|_| ())
}

/// Validates a comma-separated entry list, naming the first bad entry. What a settings save calls,
/// so the operator sees the problem while looking at it.
pub fn validate_entries(list: &str) -> Result<(), String> {
    for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        validate_entry(entry)?;
    }
    Ok(())
}

/// Splits a stored comma-separated list into validated entries. Entries a hand-edited file let in
/// that do not validate are dropped rather than fatal: an invalid entry can only ever shrink what
/// a colony reaches (allows compile last), never open something.
fn parse_entries(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .filter_map(|e| parse_entry(e).ok().map(|_| e.to_string()))
        .collect()
}

/// What one sandbox boots with: the `--net` profiles and the `--net-rule` tokens, in msb's own
/// evaluation order. `rules` is the whole story — msb walks it first-match-wins per direction.
pub(crate) struct Compiled {
    pub profiles: Vec<String>,
    pub rules: Vec<String>,
}

/// Compiles a policy against the harness's own infrastructure allows. The order is the security
/// argument: DNS first (see [`DNS_ALLOW`]), then the port-scoped infrastructure allows, then the
/// non-overridable denies, then the operator's blocks, then the operator's allows. Only the
/// harness's own rules ever precede the deny set, and allows compile last, so no configuration can
/// reopen a blocked destination.
///
/// Profiles follow the mode. `Open` keeps `public` — today's fence, whose allow rules msb appends
/// after every explicit one (crates/cli/lib/commands/common.rs `build_network_policy`, fa3e439:
/// the profile's rules are appended to the CLI's). `Allowlist` passes no `--net` at all and instead
/// sets `--net-default-egress deny`: that lands as `NetworkPolicy { default_egress: Deny,
/// default_ingress: Allow, rules: <the --net-rule tokens> }` with no profile rules. `--net none`
/// would deny ingress too (`NetworkPolicy::none()`), taking the mesh-off published port down with
/// it, so it is not the encoding. Ingress keeps the profile baseline, and DNS forwarding is
/// msb's own — an allowlist never touches either.
pub(crate) fn compile(policy: &EgressPolicy, infra: &[String]) -> Compiled {
    let mut rules = vec![DNS_ALLOW.to_string()];
    rules.extend_from_slice(infra);
    rules.extend(ALWAYS_BLOCKED.iter().map(|target| format!("deny@{target}")));
    rules.extend(policy.block.iter().map(|entry| format!("deny@{}", entry_target(entry))));
    rules.extend(policy.allow.iter().map(|entry| format!("allow@{}", entry_target(entry))));
    let profiles = match policy.mode {
        EgressMode::Open => vec!["public".to_string()],
        EgressMode::Allowlist => Vec::new(),
    };
    Compiled { profiles, rules }
}

/// The policy one org's colonies boot with: the org's mode when it set one, else the sandbox
/// module's; the allow and block lists are the union of both levels, because an org can add to a
/// fence but never subtract from one. Entries saved by neither level's validation (hand-edited
/// files) are dropped here; what is left compiles into the boot's rules.
pub fn resolve(modules: &ModulesConfig, org: &OrgSettings) -> Resolved {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    let global = |key: &str| setting_str(&modules.sandbox, &schema, key);
    let global_mode = EgressMode::parse(&global("egress")).unwrap_or_default();
    let global_allow = parse_entries(&global("egress_allow"));
    let global_block = parse_entries(&global("egress_block"));

    let over = org.egress.as_ref();
    let org_mode = over
        .and_then(|o| o.mode.as_deref())
        .filter(|m| !m.trim().is_empty())
        .and_then(|m| EgressMode::parse(m).ok());
    let org_allow: Option<Vec<String>> = over
        .and_then(|o| o.allow.as_ref())
        .map(|list| list.iter().filter(|e| parse_entry(e).is_ok()).cloned().collect());
    let org_block: Option<Vec<String>> = over
        .and_then(|o| o.block.as_ref())
        .map(|list| list.iter().filter(|e| parse_entry(e).is_ok()).cloned().collect());

    let mut sources = Sources {
        mode: match org_mode {
            Some(_) => "org".to_string(),
            None => "global".to_string(),
        },
        allow: Vec::new(),
        block: Vec::new(),
    };
    if !global_allow.is_empty() {
        sources.allow.push("global".to_string());
    }
    if !global_block.is_empty() {
        sources.block.push("global".to_string());
    }
    if org_allow.as_ref().is_some_and(|l| !l.is_empty()) {
        sources.allow.push("org".to_string());
    }
    if org_block.as_ref().is_some_and(|l| !l.is_empty()) {
        sources.block.push("org".to_string());
    }
    Resolved {
        policy: EgressPolicy {
            mode: org_mode.unwrap_or(global_mode),
            allow: union(global_allow, org_allow),
            block: union(global_block, org_block),
        },
        sources,
    }
}

fn union(global: Vec<String>, org: Option<Vec<String>>) -> Vec<String> {
    let mut out = global;
    for entry in org.into_iter().flatten() {
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

/// The record a boot leaves at `<session dir>/egress.json`: the policy that was resolved, the
/// rules it compiled to, and when. `GET /api/sessions/{id}/egress` serves it back, so the fleet
/// view can answer "what could that colony reach" without reading the boot log.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Record {
    pub mode: EgressMode,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub block: Vec<String>,
    pub sources: Sources,
    /// The deny rules every colony carries, whatever the configuration said.
    pub always_blocked: Vec<String>,
    /// The `--net-rule` tokens, in evaluation order.
    pub rules: Vec<String>,
    /// The `--net` profiles; empty in allowlist mode, which denies by default instead.
    pub profiles: Vec<String>,
    pub applied_at: u64,
}

pub(crate) fn record(resolved: &Resolved, compiled: &Compiled, applied_at: u64) -> Record {
    Record {
        mode: resolved.policy.mode,
        allow: resolved.policy.allow.clone(),
        block: resolved.policy.block.clone(),
        sources: resolved.sources.clone(),
        always_blocked: ALWAYS_BLOCKED.iter().map(|t| format!("deny@{t}")).collect(),
        rules: compiled.rules.clone(),
        profiles: compiled.profiles.clone(),
        applied_at,
    }
}

/// `GET /api/sessions/{id}/egress`: the egress record the colony's boot wrote. A colony that last
/// booted before the policy existed has none, and says so rather than reading as unfenced.
pub async fn show(State(app): State<Shared>, AxumPath(id): AxumPath<String>) -> ApiResult<serde_json::Value> {
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let bytes = tokio::fs::read(app.session_dir(&id).join("egress.json")).await.map_err(|_| {
        client_error(
            StatusCode::NOT_FOUND,
            "no egress record for this colony; it last booted before the egress policy existed",
        )
    })?;
    let record: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("corrupt egress record: {e}")))?;
    Ok(Json(record))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: EgressMode, allow: &[&str], block: &[&str]) -> EgressPolicy {
        EgressPolicy {
            mode,
            allow: allow.iter().map(|s| s.to_string()).collect(),
            block: block.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn entries_parse_into_the_tokens_msb_takes() {
        // Hosts, port-scoped and all-ports; `:0` reads as every port, like no port at all.
        assert_eq!(entry_target("api.anthropic.com"), "api.anthropic.com");
        assert_eq!(entry_target("api.anthropic.com:443"), "api.anthropic.com:tcp:443");
        assert_eq!(entry_target("api.anthropic.com:0"), "api.anthropic.com");
        assert_eq!(entry_target("*.example.com"), "suffix=example.com");
        assert_eq!(entry_target("*.example.com:443"), "suffix=example.com:tcp:443");
        // IPs and CIDRs; IPv6 keeps its brackets, and a `:0` after them still means every port.
        assert_eq!(entry_target("8.8.8.8"), "8.8.8.8");
        assert_eq!(entry_target("10.0.0.0/8"), "10.0.0.0/8");
        assert_eq!(entry_target("0.0.0.0/0"), "0.0.0.0/0");
        assert_eq!(entry_target("[2001:db8::1]"), "[2001:db8::1/128]");
        assert_eq!(entry_target("[fc00::/7]"), "[fc00::/7]");
        assert_eq!(entry_target("[2001:db8::1]:443"), "[2001:db8::1/128]:tcp:443");
        assert_eq!(entry_target("[2001:db8::1]:0"), "[2001:db8::1/128]");
        assert!(validate_entries("api.anthropic.com:443, 10.0.0.0/8 ,*.example.com").is_ok());
    }

    #[test]
    fn entries_that_could_confuse_the_rule_grammar_are_refused_by_name() {
        for bad in [
            "",
            " ",
            "@",
            "api@anthropic.com",
            "two words",
            "a,b",
            "*",
            "*.",
            "*.com",
            "no-dots",
            "host",
            "public",
            "dns",
            "meta",
            "api.com:",
            "api.com:x",
            "api.com:70000",
            "api.com:-1",
            "API.com",
            "trailing-.com",
            "-a.com",
            "[10.0.0.1]",
            "fc00::/7",
            "::1",
            "10.0.0.0/x",
            "10.0.0.0/33",
            "[fc00::/200]",
            "[fc00::",
            "[fc00::]extra",
        ] {
            assert!(parse_entry(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// The heart of the slice: whatever the two classes of configuration say, the deny set is in
    /// every compiled rule list, ahead of everything configured — and only the harness's own
    /// port-scoped infrastructure allows (plus DNS) sit before it. A deterministic LCG drives
    /// modes, entries and global/org splits, dangerous entries included, because a hand-picked
    /// example list only ever covers the cases its author thought of.
    #[test]
    fn no_configuration_reopens_the_always_blocked_set() {
        let infra = [
            "allow@192.168.1.4:udp:41743".to_string(),
            "allow@host:tcp:41740".to_string(),
            "allow@host:tcp:52000".to_string(),
        ];
        // Every one of these is something a configuration might plausibly try to open, and every
        // one parses (what a save would refuse never gets stored; see the parser test).
        let pool = [
            "169.254.169.254",
            "10.0.0.0/8",
            "192.168.0.0/16",
            "0.0.0.0/0",
            "[::]/0",
            "metadata.google.internal",
            "instance-data.ec2.internal",
            "*.ec2.internal",
            "127.0.0.1",
            "[::1]",
            "[::/0]",
            "[fc00::/7]",
            "100.64.0.0/10",
            "240.0.0.0/4",
            "registry.npmjs.org:443",
            "deb.debian.org",
        ];
        let mut state: u64 = 0x303;
        // A bare LCG: deterministic across platforms and runs, no new dependency.
        let next = |state: &mut u64, bound: usize| {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*state >> 33) as usize % bound
        };
        let pick = |state: &mut u64, n: usize| (0..n).map(|_| pool[next(state, pool.len())]).collect::<Vec<_>>();
        for round in 0..400 {
            let mode = if next(&mut state, 2) == 0 {
                EgressMode::Open
            } else {
                EgressMode::Allowlist
            };
            let entries = next(&mut state, 4);
            let allow = pick(&mut state, entries);
            let entries = next(&mut state, 4);
            let block = pick(&mut state, entries);
            let compiled = compile(&policy(mode, &allow, &block), &infra);
            let always: Vec<&str> = ALWAYS_BLOCKED.to_vec();
            // Every deny of the set is there (a configured block may restate one; that is the
            // same word, not a hole).
            for target in &always {
                assert!(
                    compiled.rules.iter().any(|r| r == &format!("deny@{target}")),
                    "round {round}: deny@{target} went missing from {:?}",
                    compiled.rules
                );
            }
            let first_deny = compiled
                .rules
                .iter()
                .position(|r| always.iter().any(|t| r == &format!("deny@{t}")))
                .unwrap();
            for (i, rule) in compiled.rules.iter().enumerate() {
                if i >= first_deny {
                    continue;
                }
                // Only DNS and the infrastructure allows may precede the deny set; in particular
                // no `allow@` compiled from `allow` or `block` can be here, whichever of the two
                // lists carried a private address, the metadata host or `0.0.0.0/0`.
                assert!(
                    rule == DNS_ALLOW || infra.contains(rule),
                    "round {round}: {rule:?} precedes the deny set in {:?}",
                    compiled.rules
                );
                assert!(
                    !rule.starts_with("deny@"),
                    "round {round}: a configured deny runs ahead of the set: {rule:?}"
                );
            }
            // And in allowlist mode nothing configured resurrects the `public` profile either.
            assert_eq!(
                compiled.profiles.is_empty(),
                mode == EgressMode::Allowlist,
                "round {round}: profiles {:?} for {mode:?}",
                compiled.profiles
            );
        }
    }

    #[test]
    fn merge_takes_the_org_mode_and_unions_the_lists() {
        let org = |mode: Option<&str>, allow: Option<Vec<&str>>, block: Option<Vec<&str>>| OrgSettings {
            egress: Some(crate::orgs::EgressOverrides {
                mode: mode.map(str::to_string),
                allow: allow.map(|l| l.into_iter().map(str::to_string).collect()),
                block: block.map(|l| l.into_iter().map(str::to_string).collect()),
            }),
            ..OrgSettings::default()
        };
        // A global setting lands as if the operator had saved it: explicit settings beat the
        // schema defaults `setting_str` falls back to.
        let set = |key: &str, value: &str| {
            let mut sandbox = ModulesConfig::default().sandbox;
            sandbox.settings.insert(key.into(), serde_json::json!(value));
            sandbox
        };
        // Global only.
        let mut global_only = ModulesConfig {
            sandbox: set("egress", "open"),
            ..ModulesConfig::default()
        };
        global_only
            .sandbox
            .settings
            .insert("egress_block".into(), serde_json::json!("10.0.0.0/8"));
        let resolved = resolve(&global_only, &OrgSettings::default());
        assert_eq!(resolved.policy.mode, EgressMode::Open);
        assert_eq!(resolved.policy.block, vec!["10.0.0.0/8"]);
        assert_eq!(resolved.sources.mode, "global");
        assert_eq!(resolved.sources.block, vec!["global"]);
        // A level that contributed no entries is not named: the record reads as whose word
        // widened the fence, and nobody's did.
        assert!(resolved.sources.allow.is_empty());
        // The org's mode wins; its lists add to, never replace, the global ones.
        let mut org_side = global_only.clone();
        org_side.sandbox = set("egress", "open");
        org_side
            .sandbox
            .settings
            .insert("egress_allow".into(), serde_json::json!("deb.debian.org"));
        org_side
            .sandbox
            .settings
            .insert("egress_block".into(), serde_json::json!("10.0.0.0/8"));
        let resolved = resolve(
            &org_side,
            &org(
                Some("allowlist"),
                Some(vec!["registry.npmjs.org:443"]),
                Some(vec!["240.0.0.0/4"]),
            ),
        );
        assert_eq!(resolved.policy.mode, EgressMode::Allowlist);
        assert_eq!(resolved.policy.allow, vec!["deb.debian.org", "registry.npmjs.org:443"]);
        assert_eq!(resolved.policy.block, vec!["10.0.0.0/8", "240.0.0.0/4"]);
        assert_eq!(resolved.sources.mode, "org");
        assert_eq!(resolved.sources.allow, vec!["global", "org"]);
        assert_eq!(resolved.sources.block, vec!["global", "org"]);
        // An org cannot drop a global block: the union stands, whatever the org omits.
        let resolved = resolve(&org_side, &org(Some("allowlist"), Some(vec!["deb.debian.org"]), None));
        assert_eq!(resolved.policy.block, vec!["10.0.0.0/8"]);
        assert_eq!(resolved.sources.block, vec!["global"]);
        // A blank org mode is inherit, like an unset one.
        let resolved = resolve(&org_side, &org(Some("  "), None, None));
        assert_eq!(resolved.policy.mode, EgressMode::Open);
        assert_eq!(resolved.sources.mode, "global");
        // A hand-edited module file carrying an unparseable entry loses the entry, not the boot.
        let mut hand_edited = org_side.clone();
        hand_edited
            .sandbox
            .settings
            .insert("egress_allow".into(), serde_json::json!("not a host"));
        assert_eq!(
            resolve(&hand_edited, &OrgSettings::default()).policy.allow,
            Vec::<String>::new()
        );
    }

    #[test]
    fn allowlist_compiles_no_public_profile_and_configured_allows_last() {
        // The TLS-edge 443 allow is the harness's own (boot.rs builds it into `infra`); a
        // configured allow is the operator's word and compiles after the deny set.
        let infra = [
            "allow@host:tcp:52000".to_string(),
            "allow@api.anthropic.com:tcp:443".to_string(),
        ];
        let compiled = compile(&policy(EgressMode::Allowlist, &["registry.npmjs.org:443"], &[]), &infra);
        assert!(
            compiled.profiles.is_empty(),
            "allowlist names no profile: {:?}",
            compiled.profiles
        );
        assert_eq!(compiled.rules[0], DNS_ALLOW);
        assert_eq!(compiled.rules[1], "allow@host:tcp:52000");
        assert_eq!(compiled.rules[2], "allow@api.anthropic.com:tcp:443");
        assert_eq!(compiled.rules[3], "deny@meta");
        let last = compiled.rules.last().unwrap();
        assert_eq!(last, "allow@registry.npmjs.org:tcp:443");
        assert!(!compiled.rules.iter().any(|r| r == "allow@public"));
        // Open keeps the profile: today's fence, with the deny set appended behind it.
        let compiled = compile(&policy(EgressMode::Open, &[], &[]), &infra);
        assert_eq!(compiled.profiles, vec!["public".to_string()]);
        assert_eq!(compiled.rules[0], DNS_ALLOW);
    }
}
