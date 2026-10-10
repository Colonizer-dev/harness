//! Plugin directories — "skillsets" in Settings — that a colony can load read-only (docs/protocol.md,
//! "Plugin directories"). A name resolves in two places, in order: what the operator put in
//! `<data>/plugins/<name>`, then what shipped with the app in `plugins/<name>`, staged there by
//! scripts/fetch-vendor.sh. Boot and the Settings list both resolve through this module, so what the
//! toggles show is what a colony will mount.

use crate::{Shared, config::Settings, util::is_plain_name};
use anyhow::{Result, anyhow, bail};
use axum::{Json, extract::State};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// The comma-separated `plugins` setting as names, in order, without blanks or repeats.
pub fn parse_list(value: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in value.split(',').map(str::trim).filter(|name| !name.is_empty()) {
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

fn local_root(cfg: &Settings) -> PathBuf {
    cfg.data_dir.join("plugins")
}

fn vendored_root(cfg: &Settings) -> Option<PathBuf> {
    cfg.assets.as_ref().map(|assets| assets.join("plugins"))
}

/// Where `name` lives, local copy first, without judging what is inside: `None` when it is not a plain
/// name or neither the data directory nor the app has it. [`resolve`] adds validation on top, and
/// [`check_skillsets`] uses the split to tell a missing skillset from a broken one.
fn locate(cfg: &Settings, name: &str) -> Option<PathBuf> {
    if !is_plain_name(name) {
        return None;
    }
    let local = local_root(cfg).join(name);
    if local.is_dir() {
        return Some(local);
    }
    vendored_root(cfg)
        .map(|vendored| vendored.join(name))
        .filter(|dir| dir.is_dir())
}

/// The directory boot mounts for `name`. A local copy overrides a vendored one of the same name, and
/// neither can be named by a path. Whichever copy wins must pass [`validate`], and every ant it
/// defines must carry only skillsets that are installed ([`check_carried_skillsets`]).
pub fn resolve(cfg: &Settings, name: &str) -> Result<PathBuf> {
    let root = local_root(cfg);
    if !is_plain_name(name) {
        bail!("plugin directory {name:?} must be a plain name under {}", root.display());
    }
    let Some(dir) = locate(cfg, name) else {
        bail!(
            "plugin directory {name:?} is not in {} or among the app's vendored plugins",
            root.display()
        );
    };
    let ants = validate_pack(&dir)?;
    check_carried_skillsets(cfg, name, &ants)?;
    Ok(dir)
}

/// The plugin manifest: the root `plugin.json` when present, otherwise the
/// legacy `.claude-plugin/plugin.json` the currently-staged packs use.
fn manifest_file(dir: &Path) -> PathBuf {
    let root = dir.join("plugin.json");
    if root.is_file() {
        root
    } else {
        dir.join(".claude-plugin/plugin.json")
    }
}

/// A legacy directory-style manifest entry (`"./skills/"`, `"skills/"`, `"."`):
/// "every skill under that directory". True when `rel` — already stripped of a
/// leading `./` and slashes — resolves under `dir` to a directory holding a
/// `SKILL.md` at most two levels beneath it (`skills/<name>/SKILL.md`, or one
/// level for a single-skill directory).
fn is_skill_tree(dir: &Path, rel: &str) -> bool {
    if rel.contains("..") {
        return false;
    }
    let base = if rel == "." { dir.to_path_buf() } else { dir.join(rel) };
    if !base.is_dir() {
        return false;
    }
    let mut stack = vec![(base, 0u8)];
    while let Some((sub, depth)) = stack.pop() {
        if sub.join("SKILL.md").is_file() {
            return true;
        }
        if depth >= 2 {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&sub) {
            stack.extend(entries.flatten().filter(|e| e.path().is_dir()).map(|e| (e.path(), depth + 1)));
        }
    }
    false
}

/// Boot-time structural validation for a plugin directory, run from
/// [`resolve`]: the manifest must exist and parse (root or legacy path),
/// every `skills/*/` directory carrying a `SKILL.md` must have a safe plain
/// name, every skill the manifest's `skills` array lists must exist on disk,
/// every `agents/*.md` must parse into a well-formed ant ([`validate_ants`]),
/// and an optional `mcp.json` must give every server a stdio `command` or a
/// remote `url` with declared hosts ([`mcp_hosts`]). A pack that fails any of
/// these blocks its colony's launch with the named error rather than mounting
/// a degraded colony. The deeper rule set — semver, SKILL.md frontmatter,
/// duplicate skill names within a pack — lives in `scripts/validate-plugins.mjs`,
/// which applies the same agent-file rules.
pub fn validate(dir: &Path) -> Result<()> {
    validate_pack(dir).map(|_| ())
}

/// [`validate`] plus the ants it parsed, for the callers that check what a pack's agents carry:
/// [`resolve`] and [`check_skillsets`] verify each ant's `skillsets` against the installed packs.
fn validate_pack(dir: &Path) -> Result<Vec<AntSpec>> {
    let manifest_path = manifest_file(dir);
    let data = match std::fs::read(&manifest_path) {
        Ok(data) => data,
        Err(_) => bail!(
            "{}: missing plugin manifest (expected plugin.json or .claude-plugin/plugin.json)",
            dir.display()
        ),
    };
    let manifest: Value = match serde_json::from_slice(&data) {
        Ok(manifest) => manifest,
        Err(err) => bail!("{}: invalid plugin manifest: {err}", manifest_path.display()),
    };
    if !manifest.is_object() {
        bail!("{}: invalid plugin manifest: expected a JSON object", manifest_path.display());
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("skills")) {
        for entry in entries.flatten() {
            if !entry.path().join("SKILL.md").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_plain_name(&name) {
                bail!("{}: skill directory {name:?} must be a plain name", dir.display());
            }
        }
    }
    if let Some(listed) = manifest.get("skills").and_then(Value::as_array) {
        for skill in listed.iter().filter_map(Value::as_str) {
            let rel = skill.trim().trim_start_matches("./").trim_matches('/');
            if rel.is_empty() || rel.contains("..") {
                bail!("{}: manifest lists invalid skill {skill:?}", manifest_path.display());
            }
            // Legacy directory-style entries (`"./skills/"`, `"skills/"`, `"."`) mean
            // "every skill under that directory" — the shape upstream ecc ships
            // (`skills: ["./skills/"]`) — so they pass when the entry resolves to a
            // directory with skills beneath it.
            if is_skill_tree(dir, rel) {
                continue;
            }
            let candidate = if rel.contains('/') {
                dir.join(rel)
            } else {
                dir.join("skills").join(rel)
            };
            if !candidate.join("SKILL.md").is_file() && !candidate.is_file() {
                bail!(
                    "{}: manifest lists skill {skill:?} but it is missing on disk",
                    manifest_path.display()
                );
            }
        }
    }
    // The mcp.json rules `scripts/validate-plugins.mjs` enforces at stage time, enforced here on
    // the pack's own `mcp.json` (not Claude Code's `.mcp.json` spelling, which this does not
    // read): boot refuses what staging would refuse.
    mcp_hosts(dir)?;
    validate_ants(dir)
}

/// The server map an `mcp.json` document carries: `mcpServers`/`servers` when either key holds an
/// object, otherwise the document itself. `None` when the document is not an object at all.
/// Mirrors `serverMap` in `scripts/validate-plugins.mjs`, so both checkers read the same shapes.
fn server_map(doc: &Value) -> Option<&Value> {
    let object = doc.as_object()?;
    for key in ["mcpServers", "servers"] {
        if let Some(map) = object.get(key).filter(|value| value.as_object().is_some()) {
            return Some(map);
        }
    }
    Some(doc)
}

/// The non-empty strings a `hosts`/`allowedHosts` declaration lists: the non-empty entries of an
/// array, or one non-empty string. Absent, `null` or any other shape declares nothing — the same
/// reading `scripts/validate-plugins.mjs` gives the field.
fn declared_hosts(hosts: Option<&Value>) -> Vec<String> {
    match hosts {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().filter(|host| !host.is_empty()))
            .map(str::to_string)
            .collect(),
        Some(Value::String(host)) if !host.is_empty() => vec![host.clone()],
        _ => Vec::new(),
    }
}

/// The hosts a pack's optional `mcp.json` declares for its servers, under the same rules
/// `scripts/validate-plugins.mjs` applies: the file parses, the server map (the document, or its
/// `mcpServers`/`servers`) is an object of server entries, every server has a non-empty stdio
/// `command` or a non-empty remote `url`, and a remote one declares a non-empty
/// `hosts`/`allowedHosts`. A pack with no `mcp.json` declares nothing.
///
/// Nothing enforces the hosts yet — colonies boot with `--net public` — but this is the reader the
/// egress allowlist of #304 consumes, so the declaration is checked where the pack is validated
/// ([`validate`]) rather than trusted later.
pub(crate) fn mcp_hosts(dir: &Path) -> Result<Vec<String>> {
    let path = dir.join("mcp.json");
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        // No tool servers: nothing declared, nothing to gate.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => bail!("{}: invalid mcp.json: {err}", path.display()),
    };
    let doc: Value = match serde_json::from_slice(&data) {
        Ok(doc) => doc,
        Err(err) => bail!("{}: invalid mcp.json: {err}", path.display()),
    };
    let Some(servers) = server_map(&doc).and_then(Value::as_object) else {
        bail!(
            "{}: mcp.json must be an object of server entries (or one under mcpServers/servers)",
            path.display()
        );
    };
    let mut hosts = Vec::new();
    for (server, config) in servers {
        let Some(cfg) = config.as_object() else {
            bail!("{}: server {server:?} must be an object", path.display());
        };
        let command = cfg.get("command").and_then(Value::as_str).is_some_and(|c| !c.is_empty());
        let url = cfg.get("url").and_then(Value::as_str).is_some_and(|u| !u.is_empty());
        // `hosts` falls through to `allowedHosts` only when absent or null, as `??` does there.
        let declared = declared_hosts(
            cfg.get("hosts")
                .filter(|value| !value.is_null())
                .or_else(|| cfg.get("allowedHosts").filter(|value| !value.is_null())),
        );
        if !command && !url {
            bail!("{}: server {server:?} needs a stdio command or a remote url", path.display());
        }
        if url && !command && declared.is_empty() {
            bail!(
                "{}: remote server {server:?} needs a non-empty hosts/allowedHosts declaration",
                path.display()
            );
        }
        hosts.extend(declared);
    }
    hosts.sort();
    hosts.dedup();
    Ok(hosts)
}

// ---- Agent files (issue #1163) --------------------------------------------------------------
//
// An ant is a Claude Code agent file — `agents/<file>.md`, frontmatter plus a system prompt —
// that optionally carries Colonizer identity in an `ant:` block and a `skillsets:` list. The
// files are read with a small YAML subset, identical to `parseAgentFrontmatter` in
// `scripts/validate-plugins.mjs`, so a pack passes or fails the same rules in CI and at boot.

/// The castes an ant can hold. A colony role is one of these (docs/protocol.md).
const ANT_CASTES: [&str; 7] = ["forager", "soldier", "weaver", "honeypot", "scout", "worker", "queen"];

/// The keys an `ant:` block may carry. Unlike the top level — where Claude Code adds fields of its
/// own over time and unknown ones are allowed — the ant block is Colonizer's, so a typo'd key is
/// an error rather than a silently ignored one.
const ANT_KEYS: [&str; 5] = ["display_name", "caste", "title", "colors", "move"];

/// One frontmatter value: the text after `key:`, and whether it was written as a double-quoted
/// string. `quoted` matters only to `ant.colors`, whose entries must be quoted so a bare `#ff0000`
/// — which YAML would read as a comment — is refused instead.
#[derive(Clone)]
struct AntField {
    value: String,
    quoted: bool,
}

/// One agent file's frontmatter: the top-level scalars and inline arrays, plus the `ant:` block's
/// own scalars when the file opens one.
#[derive(Default)]
struct AgentFrontmatter {
    scalars: BTreeMap<String, AntField>,
    arrays: BTreeMap<String, Vec<AntField>>,
    ant: Option<BTreeMap<String, AntField>>,
}

/// What an agent file carries that the cross-pack checks read: the ant's `name` and the skillsets
/// it draws on.
struct AntSpec {
    name: String,
    skillsets: Vec<String>,
}

/// `key: value` split at the first colon: the key in the plain spelling every frontmatter key here
/// uses (letters, digits, `_`, `-` — the shape `scripts/validate-plugins.mjs` matches), the value
/// trimmed.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some((key, value.trim()))
}

/// One scalar: double-quoted or bare. A leading `"` must close on the same line — there are no
/// multiline scalars in this subset.
fn scalar(raw: &str) -> Option<AntField> {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        return Some(AntField {
            value: inner.to_string(),
            quoted: true,
        });
    }
    Some(AntField {
        value: raw.to_string(),
        quoted: false,
    })
}

/// Splits on `separator`, ignoring separators inside double quotes, `[...]` and `{...}`, so a
/// nested `{ k: [a, b] }` stays one item for its consumer to reject.
fn split_items(raw: &str, separator: char) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut depth: i32 = 0;
    for ch in raw.chars() {
        if quoted {
            if ch == '"' {
                quoted = false;
            }
        } else if ch == '"' {
            quoted = true;
        } else if ch == '[' || ch == '{' {
            depth += 1;
        } else if ch == ']' || ch == '}' {
            depth -= 1;
        } else if ch == separator && depth == 0 {
            items.push(std::mem::take(&mut current));
            continue;
        }
        current.push(ch);
    }
    items.push(current);
    items
}

/// An inline array `[a, b]` of scalars; `[]` is the empty array.
fn inline_array(raw: &str) -> Option<Vec<AntField>> {
    let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    split_items(inner, ',').iter().map(|item| scalar(item)).collect()
}

/// An inline map `{ k: "v" }` of keys to scalars.
fn inline_map(raw: &str) -> Option<Vec<(String, AntField)>> {
    let inner = raw.strip_prefix('{')?.strip_suffix('}')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    split_items(inner, ',')
        .iter()
        .map(|item| key_value(item).and_then(|(key, value)| scalar(value).map(|field| (key.to_string(), field))))
        .collect()
}

/// Parses an agent file's frontmatter under the shared YAML subset: `key: value` scalars bare or
/// double-quoted, inline arrays `[a, b]` and inline maps `{ k: "v" }`, and one nested block level
/// at exactly two spaces of indent, which only `ant:` opens. Blank lines are skipped; any other
/// line this cannot read is an error naming the file and the line's number.
fn parse_agent_frontmatter(path: &Path, text: &str) -> Result<AgentFrontmatter> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|line| line.trim()) != Some("---") {
        bail!("{}: agent files need a leading --- frontmatter block", path.display());
    }
    // The index within `lines` of the closing fence (`position` counts from the skip, so add the
    // opening fence back).
    let Some(end) = lines
        .iter()
        .skip(1)
        .position(|line| line.trim() == "---")
        .map(|index| index + 1)
    else {
        bail!("{}: the frontmatter is missing its closing ---", path.display());
    };
    let mut parsed = AgentFrontmatter::default();
    for (offset, line) in lines[1..end].iter().enumerate() {
        let number = offset + 2; // 1-based, counting the opening ---.
        if line.trim().is_empty() {
            continue;
        }
        // Indented lines belong to the `ant:` block, at exactly two spaces and nothing else.
        if line.starts_with(' ') || line.starts_with('\t') {
            if !line.starts_with("  ") || line.starts_with("   ") {
                bail!(
                    "{}: line {number}: the ant: block is indented exactly two spaces",
                    path.display()
                );
            }
            let Some(block) = parsed.ant.as_mut() else {
                bail!("{}: line {number}: only ant: opens an indented block", path.display());
            };
            let (key, raw) =
                key_value(&line[2..]).ok_or_else(|| anyhow!("{}: line {number}: cannot parse that line", path.display()))?;
            let field = if raw.is_empty() {
                AntField {
                    value: String::new(),
                    quoted: false,
                }
            } else {
                scalar(raw).ok_or_else(|| anyhow!("{}: line {number}: cannot parse that line", path.display()))?
            };
            block.insert(key.to_string(), field);
            continue;
        }
        let (key, raw) = key_value(line).ok_or_else(|| anyhow!("{}: line {number}: cannot parse that line", path.display()))?;
        if raw.is_empty() {
            // `ant:` opens the nested block; any other bare `key:` is an empty scalar, which the
            // required-field rules refuse where emptiness matters.
            if key == "ant" {
                parsed.ant = Some(BTreeMap::new());
            } else {
                parsed.scalars.insert(
                    key.to_string(),
                    AntField {
                        value: String::new(),
                        quoted: false,
                    },
                );
            }
            continue;
        }
        if key == "ant" {
            // An inline `ant: { display_name: "Sarge", caste: forager }` reads like the block
            // form; anything else spelled after `ant:` cannot be one.
            let entries = inline_map(raw).ok_or_else(|| {
                anyhow!(
                    "{}: line {number}: ant: must be a block indented two spaces or an inline map",
                    path.display()
                )
            })?;
            parsed.ant = Some(entries.into_iter().collect());
            continue;
        }
        if raw.starts_with('[') {
            let items = inline_array(raw).ok_or_else(|| anyhow!("{}: line {number}: cannot parse that line", path.display()))?;
            parsed.arrays.insert(key.to_string(), items);
            continue;
        }
        let field = scalar(raw).ok_or_else(|| anyhow!("{}: line {number}: cannot parse that line", path.display()))?;
        parsed.scalars.insert(key.to_string(), field);
    }
    Ok(parsed)
}

/// Whether `value` is a `#rrggbb` color.
fn is_hex_color(value: &str) -> bool {
    value
        .strip_prefix('#')
        .is_some_and(|digits| digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `ant.colors`, when present, is exactly `{ body: "#rrggbb", dark: "#rrggbb", accent: "#rrggbb" }`:
/// the three keys and nothing else, each a double-quoted six-digit hex string.
fn check_ant_colors(path: &Path, colors: &AntField) -> Result<()> {
    let entries = inline_map(&colors.value).ok_or_else(|| {
        anyhow!(
            "{}: ant.colors must be an inline map {{ body: \"#rrggbb\", dark: \"#rrggbb\", accent: \"#rrggbb\" }}",
            path.display()
        )
    })?;
    let mut seen = BTreeSet::new();
    for (key, field) in entries {
        if !matches!(key.as_str(), "body" | "dark" | "accent") {
            bail!("{}: unknown ant color {key:?} (known: body, dark, accent)", path.display());
        }
        if !field.quoted || !is_hex_color(&field.value) {
            bail!(
                "{}: ant.colors.{key} must be a double-quoted #rrggbb hex string",
                path.display()
            );
        }
        seen.insert(key);
    }
    for key in ["body", "dark", "accent"] {
        if !seen.contains(key) {
            bail!("{}: ant.colors must list {key}: (body, dark, accent)", path.display());
        }
    }
    Ok(())
}

/// The Colonizer `ant:` block: `display_name` and `caste` are required (the caste one of
/// [`ANT_CASTES`]), `title`, `colors` and `move` are optional strings, and unknown keys are
/// refused — a typo'd ant field would otherwise silently strip the ant of its identity.
fn check_ant_block(path: &Path, ant: &BTreeMap<String, AntField>) -> Result<()> {
    for key in ant.keys() {
        if !ANT_KEYS.contains(&key.as_str()) {
            bail!(
                "{}: unknown ant field {key:?} (known: {})",
                path.display(),
                ANT_KEYS.join(", ")
            );
        }
    }
    match ant.get("display_name") {
        Some(field) if !field.value.trim().is_empty() => {}
        _ => bail!("{}: the ant block needs a non-empty display_name:", path.display()),
    }
    match ant.get("caste").map(|field| field.value.as_str()) {
        Some(caste) if ANT_CASTES.contains(&caste) => {}
        None | Some("") => {
            bail!(
                "{}: the ant block needs a caste: one of {}",
                path.display(),
                ANT_CASTES.join(", ")
            )
        }
        Some(caste) => bail!("{}: unknown ant caste {caste:?}", path.display()),
    }
    for key in ["title", "move"] {
        if let Some(field) = ant.get(key)
            && field.value.trim().is_empty()
        {
            bail!("{}: ant.{key} must be a non-empty string", path.display());
        }
    }
    if let Some(colors) = ant.get("colors") {
        check_ant_colors(path, colors)?;
    }
    Ok(())
}

/// Parses and judges one agent file: the frontmatter must be present and terminated, `name` and
/// `description` are required (the name a plain name), and the Colonizer fields — `tools`,
/// `disallowedTools`, `skillsets`, `model`, `effort`, and the `ant:` block — are checked when
/// present. Plain Claude Code frontmatter passes unchanged: unknown top-level keys are Claude
/// Code's to add, and only Colonizer's own fields are judged.
fn parse_agent_file(path: &Path, text: &str) -> Result<AntSpec> {
    let parsed = parse_agent_frontmatter(path, text)?;
    let Some(name) = parsed.scalars.get("name").filter(|field| !field.value.trim().is_empty()) else {
        bail!("{}: agents need a non-empty name:", path.display());
    };
    if !is_plain_name(&name.value) {
        bail!("{}: agent name {:?} must be a plain name", path.display(), name.value);
    }
    match parsed.scalars.get("description") {
        Some(field) if !field.value.trim().is_empty() => {}
        _ => bail!("{}: agents need a non-empty description:", path.display()),
    }
    for key in ["tools", "disallowedTools"] {
        if let Some(items) = parsed.arrays.get(key)
            && items.iter().any(|item| item.value.trim().is_empty())
        {
            bail!("{}: {key}: entries must be non-empty strings", path.display());
        }
    }
    let mut skillsets = Vec::new();
    if let Some(items) = parsed.arrays.get("skillsets") {
        for item in items {
            if !is_plain_name(&item.value) {
                bail!("{}: skillset {:?} is not a plain name", path.display(), item.value);
            }
            skillsets.push(item.value.clone());
        }
    }
    for key in ["model", "effort"] {
        if parsed.arrays.contains_key(key) {
            bail!("{}: {key}: must be a string", path.display());
        }
    }
    if let Some(ant) = parsed.ant.as_ref() {
        check_ant_block(path, ant)?;
    }
    Ok(AntSpec {
        name: name.value.clone(),
        skillsets,
    })
}

/// The agents half of [`validate_pack`]: every `agents/*.md` must parse into a well-formed ant,
/// and one pack cannot define the same ant name twice — Claude Code addresses an agent by name,
/// so two files answering to `sarge` in one pack are ambiguous by construction.
fn validate_ants(dir: &Path) -> Result<Vec<AntSpec>> {
    let mut ants = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir.join("agents")) else {
        return Ok(ants);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let ant = parse_agent_file(&path, &std::fs::read_to_string(&path)?)?;
        if let Some(first) = seen.insert(ant.name.to_lowercase(), file.clone()) {
            bail!(
                "{}: agent {:?} is defined by both agents/{first} and agents/{file}: agent names must be unique within a pack",
                dir.display(),
                ant.name
            );
        }
        ants.push(ant);
    }
    Ok(ants)
}

/// Every `agents/*.md` in a pack, as the ant each defines. Files that do not parse are skipped:
/// [`validate`] has already refused any pack carrying one, so this lenient read serves only the
/// cross-pack checks, which run after every enabled pack has passed.
fn ants(dir: &Path) -> Vec<AntSpec> {
    let Ok(entries) = std::fs::read_dir(dir.join("agents")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_file() && entry.path().extension().is_some_and(|ext| ext == "md"))
        .filter_map(|entry| {
            let path = entry.path();
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_agent_file(&path, &text).ok())
        })
        .collect()
}

/// The skillsets an ant carries must be installed: `skillsets:` names the packs the ant draws on,
/// and an ant pointing at a pack neither the operator's data directory nor the app has would mount
/// a colony missing its own crew's tools. `resolve` runs this per pack, and so does
/// [`check_skillsets`] at save time, so a name is never accepted that boot would refuse.
fn check_carried_skillsets(cfg: &Settings, pack: &str, ants: &[AntSpec]) -> Result<()> {
    for ant in ants {
        for skillset in &ant.skillsets {
            if locate(cfg, skillset).is_none() {
                bail!(
                    "ant {:?} in skillset {pack:?} carries unknown skillset {skillset:?}; available: {}",
                    ant.name,
                    known_skillsets(cfg).join(", ")
                );
            }
        }
    }
    Ok(())
}

/// The agent names across the enabled packs must be unique, the way skill names must be
/// (issue #1163): Claude Code loads `agents/<file>.md` by name, so two packs defining an ant
/// called `sarge` are ambiguous by construction. A collision blocks boot with both packs named.
pub fn check_ant_uniqueness(packs: &[(&str, PathBuf)]) -> Result<()> {
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for (pack, dir) in packs {
        for ant in ants(dir) {
            if let Some(first) = owner.insert(ant.name.clone(), *pack) {
                bail!(
                    "agent {:?} is defined by both {first:?} and {pack:?}: agent names must be unique across enabled packs",
                    ant.name
                );
            }
        }
    }
    Ok(())
}

/// Skill names across the enabled packs must be unique: the model addresses a
/// skill as `<pack>:<name>`, so two packs answering to the same name are
/// ambiguous by construction. A collision blocks boot with both packs named.
/// What counts is what Claude Code loads — `skills/<name>/SKILL.md` on disk —
/// not what each manifest lists.
pub fn check_skill_uniqueness(packs: &[(&str, PathBuf)]) -> Result<()> {
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for (pack, dir) in packs {
        let Ok(entries) = std::fs::read_dir(dir.join("skills")) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().join("SKILL.md").is_file() {
                continue;
            }
            let Ok(skill) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(first) = owner.insert(skill.clone(), *pack) {
                bail!("skill {skill:?} is in both {first:?} and {pack:?}: skill names must be unique across enabled packs");
            }
        }
    }
    Ok(())
}

/// The vendored copy of `name` that a local copy hides, if both exist, so boot can say which one it
/// mounted instead of letting the local copy win silently.
pub fn shadowed_vendored(cfg: &Settings, name: &str) -> Option<PathBuf> {
    if !is_plain_name(name) || !local_root(cfg).join(name).is_dir() {
        return None;
    }
    vendored_root(cfg).map(|root| root.join(name)).filter(|dir| dir.is_dir())
}

/// The shadow line: which copy a colony gets, and which one it does not (#326).
fn shadow_message(name: &str, local: &Path, vendored: &Path) -> String {
    format!(
        "plugin {name:?}: local copy at {} shadows the vendored copy at {} (local wins)",
        local.display(),
        vendored.display()
    )
}

/// Refuses, at save time, any name `resolve` would refuse at boot. A name that is not there is refused
/// listing what could be named instead; one that is there but fails [`validate`] is refused with the
/// validation error, which names the file and the rule, since listing it as "available" would not help.
pub fn check_skillsets<'a>(cfg: &Settings, names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    for name in names {
        let Some(dir) = locate(cfg, name) else {
            return Err(unknown_skillset(cfg, name));
        };
        // Saving is when a shadow is chosen: name the loser here, as each colony boot does in its
        // own log (sessions.rs), instead of leaving it to the list API's `shadows_vendored` flag.
        if let Some(vendored) = shadowed_vendored(cfg, name) {
            eprintln!("{}", shadow_message(name, &dir, &vendored));
        }
        let ants = validate_pack(&dir).map_err(|err| format!("skillset {name:?} is invalid: {err:#}"))?;
        // The same rule resolve applies at boot: an ant may only carry installed skillsets, so a
        // save never accepts a pack whose crew would block the colony's next launch.
        check_carried_skillsets(cfg, name, &ants).map_err(|err| format!("skillset {name:?} is invalid: {err:#}"))?;
    }
    Ok(())
}

/// The names both install places could provide, sorted and deduplicated: what an "unknown" error
/// offers as the things that could have been named instead.
fn known_skillsets(cfg: &Settings) -> Vec<String> {
    let mut known: Vec<String> = directories(&local_root(cfg)).into_keys().collect();
    known.extend(vendored_root(cfg).into_iter().flat_map(|root| directories(&root).into_keys()));
    known.sort();
    known.dedup();
    known
}

fn unknown_skillset(cfg: &Settings, name: &str) -> String {
    let known = known_skillsets(cfg);
    let available = if known.is_empty() {
        "none".to_string()
    } else {
        known.join(", ")
    };
    format!("unknown skillset {name:?}; available: {available}")
}

/// Entries directly under `dir` that `keep` accepts; 0 when the directory is absent.
fn count(dir: &Path, keep: impl Fn(&Path) -> bool) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().filter(|entry| keep(&entry.path())).count())
        .unwrap_or(0)
}

/// What a switch in Settings needs to say about one plugin directory. The counts are the context cost
/// of switching it on: Claude Code discovers `skills/<name>/SKILL.md`, `agents/*.md` and `commands/*.md`.
fn describe(name: &str, dir: &Path, source: &str, shadows_vendored: bool) -> Value {
    let manifest: Value = std::fs::read(manifest_file(dir))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
    let markdown = |path: &Path| path.is_file() && path.extension().is_some_and(|ext| ext == "md");
    json!({
        "name": name,
        "description": manifest["description"].as_str(),
        "version": manifest["version"].as_str(),
        "source": source,
        "shadows_vendored": shadows_vendored,
        "skills": count(&dir.join("skills"), |path| path.join("SKILL.md").is_file()),
        "agents": count(&dir.join("agents"), markdown),
        "commands": count(&dir.join("commands"), markdown),
    })
}

/// Plain-named subdirectories of `root`, by name.
fn directories(root: &Path) -> BTreeMap<String, PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, _)| is_plain_name(name))
        .collect()
}

/// The locally-installed packs as `(name, dir)`, sorted by name. The upload manifest
/// (upload.rs) lists these by name and version only — never a pack's contents.
pub(crate) fn local_packs(cfg: &Settings) -> Vec<(String, PathBuf)> {
    directories(&local_root(cfg)).into_iter().collect()
}

/// A pack's manifest `version`, when the manifest exists and parses.
pub(crate) fn manifest_version(dir: &Path) -> Option<String> {
    let manifest: Value = std::fs::read(manifest_file(dir))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())?;
    manifest["version"].as_str().map(str::to_string)
}

/// Every plugin directory a colony could load, one entry per name, sorted.
pub fn available(cfg: &Settings) -> Value {
    let root = local_root(cfg);
    let vendored = vendored_root(cfg).map(|dir| directories(&dir)).unwrap_or_default();
    let mut plugins: BTreeMap<String, Value> = vendored
        .iter()
        .map(|(name, dir)| (name.clone(), describe(name, dir, "vendored", false)))
        .collect();
    for (name, dir) in directories(&root) {
        let shadows = vendored.contains_key(&name);
        plugins.insert(name.clone(), describe(&name, &dir, "local", shadows));
    }
    json!({"local_root": root, "plugins": plugins.into_values().collect::<Vec<_>>()})
}

/// `GET /api/plugins`: the skillsets on disk, plus the ones that can be downloaded and where each download is.
pub async fn list(State(app): State<Shared>) -> Json<Value> {
    let mut body = available(&app.cfg);
    body["downloadable"] = json!([
        crate::graft::current(&app).await,
        crate::understand_anything::current(&app).await
    ]);
    Json(body)
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/plugins", routing::get(list))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(dir: &Path, version: &str, skills: &[&str], agents: &[&str]) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/plugin.json"),
            json!({"name": "x", "version": version, "description": "d"}).to_string(),
        )
        .unwrap();
        for skill in skills {
            std::fs::create_dir_all(dir.join("skills").join(skill)).unwrap();
            std::fs::write(dir.join("skills").join(skill).join("SKILL.md"), "---\n").unwrap();
        }
        // A skill directory without SKILL.md is not a skill Claude Code would load.
        std::fs::create_dir_all(dir.join("skills/not-a-skill")).unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        for agent in agents {
            std::fs::write(
                dir.join("agents").join(format!("{agent}.md")),
                format!("---\nname: {agent}\ndescription: an ant\n---\n"),
            )
            .unwrap();
        }
        std::fs::write(dir.join("agents/README.txt"), "").unwrap();
    }

    /// A full ant file: `name`/`description` plus whatever `extra` spells after them, and a
    /// system prompt body behind the closing fence.
    fn ant_md(name: &str, extra: &[&str]) -> String {
        let mut front = vec![format!("name: {name}"), "description: Runs the crew".to_string()];
        front.extend(extra.iter().map(|line| line.to_string()));
        format!("---\n{}\n---\n\nDo the work.\n", front.join("\n"))
    }

    fn settings(root: &Path, assets: Option<PathBuf>) -> Settings {
        Settings {
            bind: "127.0.0.1:0".into(),
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            runtime_dir: root.join("run"),
            assets,
            msb: "msb".into(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: vec![],
            fleet_peers: vec![],
            bench_pool: None,
        }
    }

    /// A data directory and an app directory, as a real install lays them out. Returns the temp root to
    /// remove afterwards.
    fn install() -> (PathBuf, Settings) {
        let root = std::env::temp_dir().join(format!("colonizer-plugins-test-{}", crate::util::short_id()));
        let app = root.join("app");
        std::fs::create_dir_all(app.join("plugins")).unwrap();
        let cfg = settings(&root, Some(app));
        std::fs::create_dir_all(cfg.data_dir.join("plugins")).unwrap();
        (root, cfg)
    }

    fn write_manifest(dir: &Path, at_root: bool, manifest: Value) {
        let path = if at_root {
            dir.join("plugin.json")
        } else {
            dir.join(".claude-plugin/plugin.json")
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, manifest.to_string()).unwrap();
    }

    #[test]
    fn a_legacy_directory_style_skills_entry_means_every_skill_beneath_it() {
        let (root, cfg) = install();
        // The exact shape upstream ecc ships: directory entries, not skill names.
        let dir = cfg.data_dir.join("plugins/ecc-shape");
        plugin(&dir, "2.2.1", &["tdd"], &[]);
        write_manifest(
            &dir,
            false,
            json!({"name": "ecc", "version": "2.2.1", "description": "d",
                   "skills": ["./skills/"], "commands": ["./commands/"]}),
        );
        assert!(resolve(&cfg, "ecc-shape").is_ok(), "ecc must keep booting unchanged");
        // ...but a directory entry with no skills beneath it still fails.
        let dir = cfg.data_dir.join("plugins/empty-shape");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::remove_dir_all(dir.join("skills/not-a-skill")).unwrap();
        write_manifest(
            &dir,
            false,
            json!({"name": "x", "version": "1.0.0", "description": "d", "skills": ["./skills/"]}),
        );
        assert!(resolve(&cfg, "empty-shape").is_err(), "an empty skills tree is not a skill");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_skill_in_two_enabled_packs_fails_naming_both_packs() {
        let (root, cfg) = install();
        let a = cfg.data_dir.join("plugins/pack-a");
        let b = cfg.data_dir.join("plugins/pack-b");
        plugin(&a, "1.0.0", &["shared", "only-a"], &[]);
        plugin(&b, "1.0.0", &["shared", "only-b"], &[]);
        let err = check_skill_uniqueness(&[("pack-a", a), ("pack-b", b)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("pack-a") && err.contains("pack-b"), "names both packs: {err}");
        assert!(err.contains("shared"), "names the skill: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_agent_file_without_frontmatter_is_refused_naming_the_file() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/no-fm");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(dir.join("agents/broken.md"), "Just a body, no frontmatter.\n").unwrap();
        let err = resolve(&cfg, "no-fm").unwrap_err().to_string();
        assert!(err.contains("agents/broken.md"), "names the file: {err}");
        assert!(err.contains("leading ---"), "names the rule: {err}");
        // An unterminated block fails at the closing fence.
        std::fs::write(dir.join("agents/broken.md"), "---\nname: broken\n").unwrap();
        let err = resolve(&cfg, "no-fm").unwrap_err().to_string();
        assert!(err.contains("closing ---"), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_plain_claude_code_agent_passes_and_unknown_top_level_keys_are_allowed() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/plain-agents");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(
            dir.join("agents/planner.md"),
            "---\nname: planner\ndescription: Plans the work\ntools: [Read, Grep]\ndisallowedTools: [Edit]\n\
             model: sonnet\neffort: high\nsomething-new: whatever Claude Code adds next\n---\n\nPlan.\n",
        )
        .unwrap();
        assert!(resolve(&cfg, "plain-agents").is_ok(), "plain frontmatter keeps validating");
        // The Colonizer fields are judged only when present: an array where a string belongs,
        // or a skillset name that is not a plain name, each fail naming the field.
        std::fs::write(dir.join("agents/planner.md"), ant_md("planner", &["model: [sonnet]"])).unwrap();
        let err = resolve(&cfg, "plain-agents").unwrap_err().to_string();
        assert!(err.contains("model: must be a string"), "{err}");
        std::fs::write(dir.join("agents/planner.md"), ant_md("planner", &["skillsets: [../escape]"])).unwrap();
        let err = resolve(&cfg, "plain-agents").unwrap_err().to_string();
        assert!(err.contains("not a plain name"), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_whitespace_only_quoted_name_is_refused_as_empty() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/blank-name");
        plugin(&dir, "1.0.0", &[], &[]);
        // `name: " "` survives `is_empty` but the runner's own parser trims the value away, so boot
        // must refuse it exactly like a missing name (the JS validator already does).
        std::fs::write(dir.join("agents/sarge.md"), ant_md("\" \"", &[])).unwrap();
        let err = resolve(&cfg, "blank-name").unwrap_err().to_string();
        assert!(err.contains("agents/sarge.md"), "names the file: {err}");
        assert!(
            err.contains("agents need a non-empty name:"),
            "the standard name error: {err}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_ant_with_a_broken_block_is_refused_with_the_field_named() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/castes");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(
            dir.join("agents/sarge.md"),
            ant_md("sarge", &["ant:", "  display_name: Sarge", "  caste: general"]),
        )
        .unwrap();
        let err = resolve(&cfg, "castes").unwrap_err().to_string();
        assert!(err.contains("unknown ant caste \"general\""), "names the caste: {err}");

        // The ant block is Colonizer's, so a typo'd key is an error, not a silent ignore.
        std::fs::write(
            dir.join("agents/sarge.md"),
            ant_md(
                "sarge",
                &["ant:", "  display_name: Sarge", "  caste: forager", "  colour: red"],
            ),
        )
        .unwrap();
        let err = resolve(&cfg, "castes").unwrap_err().to_string();
        assert!(err.contains("unknown ant field \"colour\""), "{err}");

        std::fs::write(dir.join("agents/sarge.md"), ant_md("sarge", &["ant:", "  caste: forager"])).unwrap();
        let err = resolve(&cfg, "castes").unwrap_err().to_string();
        assert!(err.contains("non-empty display_name"), "{err}");

        std::fs::write(
            dir.join("agents/sarge.md"),
            ant_md(
                "sarge",
                &[
                    "ant:",
                    "  display_name: Sarge",
                    "  caste: forager",
                    "  colors: { body: \"#112233\", dark: #445566, accent: \"#778899\" }",
                ],
            ),
        )
        .unwrap();
        let err = resolve(&cfg, "castes").unwrap_err().to_string();
        assert!(
            err.contains("ant.colors.dark must be a double-quoted #rrggbb hex string"),
            "{err}"
        );

        std::fs::write(
            dir.join("agents/sarge.md"),
            ant_md(
                "sarge",
                &[
                    "ant:",
                    "  display_name: Sarge",
                    "  caste: forager",
                    "  colors: { body: \"#112233\", dark: \"#445566\" }",
                ],
            ),
        )
        .unwrap();
        let err = resolve(&cfg, "castes").unwrap_err().to_string();
        assert!(err.contains("ant.colors must list accent"), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unparsable_frontmatter_line_is_named_with_its_number() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/junk-frontmatter");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(
            dir.join("agents/junk.md"),
            "---\nname: junk\ndescription: d\nnot a frontmatter line\n---\n",
        )
        .unwrap();
        let err = resolve(&cfg, "junk-frontmatter").unwrap_err().to_string();
        assert!(err.contains("agents/junk.md: line 4"), "names the file and the line: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_agent_names_within_one_pack_are_refused() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/twins");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(dir.join("agents/sarge.md"), ant_md("sarge", &[])).unwrap();
        std::fs::write(dir.join("agents/again.md"), ant_md("sarge", &[])).unwrap();
        let err = resolve(&cfg, "twins").unwrap_err().to_string();
        assert!(err.contains("both agents/sarge.md and agents/again.md"), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_ant_carrying_an_uninstalled_skillset_fails_naming_ant_pack_and_skillset() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/crew");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(
            dir.join("agents/sarge.md"),
            ant_md(
                "sarge",
                &[
                    "skillsets: [ponytail, ecc]",
                    "ant:",
                    "  display_name: Sarge",
                    "  caste: soldier",
                ],
            ),
        )
        .unwrap();
        let err = resolve(&cfg, "crew").unwrap_err().to_string();
        assert!(
            err.contains("ant \"sarge\" in skillset \"crew\" carries unknown skillset \"ponytail\""),
            "names the ant, the pack and the skillset: {err}"
        );
        // Installed — vendored or local — the ant's crew is there, and the pack boots.
        plugin(&cfg.assets.clone().unwrap().join("plugins/ponytail"), "1.0.0", &[], &[]);
        plugin(&cfg.assets.clone().unwrap().join("plugins/ecc"), "2.2.1", &[], &[]);
        assert!(resolve(&cfg, "crew").is_ok());
        // The save-time gate refuses the same pack with the same rule.
        std::fs::remove_dir_all(cfg.assets.clone().unwrap().join("plugins/ponytail")).unwrap();
        let err = check_skillsets(&cfg, ["crew"]).unwrap_err();
        assert!(err.starts_with("skillset \"crew\" is invalid: "), "{err}");
        assert!(err.contains("carries unknown skillset \"ponytail\""), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_agent_name_in_two_enabled_packs_fails_naming_both_packs() {
        let (root, cfg) = install();
        let a = cfg.data_dir.join("plugins/crew-a");
        let b = cfg.data_dir.join("plugins/crew-b");
        plugin(&a, "1.0.0", &[], &["shared"]);
        plugin(&b, "1.0.0", &[], &["shared", "only-b"]);
        let err = check_ant_uniqueness(&[("crew-a", a.clone()), ("crew-b", b.clone())])
            .unwrap_err()
            .to_string();
        assert!(err.contains("crew-a") && err.contains("crew-b"), "names both packs: {err}");
        assert!(err.contains("agent \"shared\""), "names the agent: {err}");
        // Distinct names pass; the check runs on the packs boot resolved.
        std::fs::remove_file(b.join("agents/shared.md")).unwrap();
        assert!(check_ant_uniqueness(&[("crew-a", a), ("crew-b", b)]).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_manifest_listed_skill_missing_on_disk_fails_with_a_named_error() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/ghost-pack");
        plugin(&dir, "1.0.0", &["real"], &[]);
        write_manifest(
            &dir,
            false,
            json!({"name": "x", "version": "1.0.0", "description": "d", "skills": ["skills/real", "ghost"]}),
        );
        let err = resolve(&cfg, "ghost-pack").unwrap_err().to_string();
        assert!(err.contains("ghost"), "names the skill: {err}");
        assert!(err.contains("plugin.json"), "names the file: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unsafe_skill_directory_name_fails_validation() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/unsafe-pack");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::create_dir_all(dir.join("skills/.hidden")).unwrap();
        std::fs::write(dir.join("skills/.hidden/SKILL.md"), "---\n").unwrap();
        let err = resolve(&cfg, "unsafe-pack").unwrap_err().to_string();
        assert!(err.contains(".hidden"), "names the directory: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_remote_mcp_server_without_hosts_fails_naming_the_rule() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/remote-pack");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(dir.join("mcp.json"), r#"{"greeter": {"url": "https://api.example/mcp"}}"#).unwrap();
        let err = resolve(&cfg, "remote-pack").unwrap_err().to_string();
        assert!(err.contains("mcp.json"), "names the file: {err}");
        assert!(
            err.contains("remote server \"greeter\" needs a non-empty hosts"),
            "names the rule: {err}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_remote_mcp_server_with_hosts_passes_and_mcp_hosts_lists_them() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/remote-pack");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(
            dir.join("mcp.json"),
            r#"{"mcpServers": {
                "a": {"url": "https://api.example/mcp", "hosts": ["api.example", ""]},
                "b": {"url": "https://b.example/mcp", "allowedHosts": "b.example"}}}"#,
        )
        .unwrap();
        assert!(resolve(&cfg, "remote-pack").is_ok());
        // Both spellings read, empty entries dropped, the result sorted and deduplicated.
        assert_eq!(mcp_hosts(&dir).unwrap(), ["api.example", "b.example"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_stdio_mcp_server_passes_and_a_pack_without_mcp_json_passes() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/stdio-pack");
        plugin(&dir, "1.0.0", &[], &[]);
        assert!(resolve(&cfg, "stdio-pack").is_ok(), "a pack with no mcp.json at all is fine");
        std::fs::write(
            dir.join("mcp.json"),
            r#"{"mcpServers": {"local": {"command": "node", "args": ["server.js"]}}}"#,
        )
        .unwrap();
        assert!(resolve(&cfg, "stdio-pack").is_ok());
        assert_eq!(mcp_hosts(&dir).unwrap(), Vec::<String>::new());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_malformed_mcp_json_fails_naming_the_file() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/broken-mcp");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::write(dir.join("mcp.json"), "{broken").unwrap();
        let err = resolve(&cfg, "broken-mcp").unwrap_err().to_string();
        assert!(err.contains("mcp.json"), "names the file: {err}");
        assert!(err.contains("invalid mcp.json"), "names the rule: {err}");
        // A server that is neither stdio nor remote has its own rule.
        std::fs::write(dir.join("mcp.json"), r#"{"mcpServers": {"weird": {"args": []}}}"#).unwrap();
        let err = resolve(&cfg, "broken-mcp").unwrap_err().to_string();
        assert!(err.contains("needs a stdio command or a remote url"), "names the rule: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_root_manifest_is_preferred_over_the_legacy_one() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/rooted");
        plugin(&dir, "1.0.0-legacy", &[], &[]);
        write_manifest(&dir, true, json!({"name": "x", "version": "2.0.0-root", "description": "d"}));
        // The legacy manifest is broken, but the root one carries the pack.
        std::fs::write(dir.join(".claude-plugin/plugin.json"), "{broken").unwrap();
        assert!(resolve(&cfg, "rooted").is_ok());
        let plugins = available(&cfg)["plugins"].as_array().unwrap().clone();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["version"], "2.0.0-root");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_lists_parse_in_order_without_blanks_or_repeats() {
        assert_eq!(
            parse_list(" ecc, ,superpowers,ecc,google-skills "),
            ["ecc", "superpowers", "google-skills"]
        );
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn the_shadow_line_names_the_winner_and_the_loser() {
        assert_eq!(
            shadow_message("ecc", Path::new("/data/plugins/ecc"), Path::new("/app/plugins/ecc")),
            "plugin \"ecc\": local copy at /data/plugins/ecc shadows the vendored copy at /app/plugins/ecc (local wins)"
        );
    }

    #[test]
    fn a_local_copy_overrides_a_vendored_one_and_paths_are_refused() {
        let (root, cfg) = install();
        let vendored = cfg.assets.clone().unwrap().join("plugins/ecc");
        plugin(&vendored, "2.2.1", &["tdd"], &[]);
        assert_eq!(resolve(&cfg, "ecc").unwrap(), vendored);

        let local = cfg.data_dir.join("plugins/ecc");
        plugin(&local, "9.9.9", &[], &[]);
        assert_eq!(resolve(&cfg, "ecc").unwrap(), local);

        assert!(resolve(&cfg, "missing").is_err());
        for bad in ["../ecc", "a/b", ".hidden", ""] {
            assert!(resolve(&cfg, bad).is_err(), "{bad:?} must be refused");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_local_copy_that_shadows_a_vendored_one_is_reported() {
        let (root, cfg) = install();
        let vendored = cfg.assets.clone().unwrap().join("plugins/ecc");
        plugin(&vendored, "2.2.1", &[], &[]);
        assert_eq!(
            shadowed_vendored(&cfg, "ecc"),
            None,
            "nothing is shadowed without a local copy"
        );

        plugin(&cfg.data_dir.join("plugins/ecc"), "9.9.9", &[], &[]);
        plugin(&cfg.data_dir.join("plugins/team-skills"), "1.0.0", &[], &[]);
        assert_eq!(shadowed_vendored(&cfg, "ecc"), Some(vendored));
        assert_eq!(
            shadowed_vendored(&cfg, "team-skills"),
            None,
            "a local-only copy shadows nothing"
        );
        assert_eq!(shadowed_vendored(&cfg, "../ecc"), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_skillsets_are_refused_with_what_is_available() {
        let (root, cfg) = install();
        assert_eq!(
            check_skillsets(&cfg, ["ecc"]).unwrap_err(),
            "unknown skillset \"ecc\"; available: none"
        );

        plugin(&cfg.assets.clone().unwrap().join("plugins/ecc"), "2.2.1", &[], &[]);
        plugin(&cfg.assets.clone().unwrap().join("plugins/superpowers"), "6.3.0", &[], &[]);
        plugin(&cfg.data_dir.join("plugins/ecc"), "9.9.9", &[], &[]);
        plugin(&cfg.data_dir.join("plugins/team-skills"), "1.0.0", &[], &[]);
        assert_eq!(check_skillsets(&cfg, ["ecc", "superpowers", "team-skills"]), Ok(()));
        assert_eq!(check_skillsets(&cfg, []), Ok(()));
        assert_eq!(
            check_skillsets(&cfg, ["ecc", "ec"]).unwrap_err(),
            "unknown skillset \"ec\"; available: ecc, superpowers, team-skills"
        );
        assert!(
            check_skillsets(&cfg, ["../ecc"])
                .unwrap_err()
                .starts_with("unknown skillset \"../ecc\"")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_existing_but_invalid_skillset_is_refused_with_the_validation_error() {
        let (root, cfg) = install();
        // There, but with no manifest at either path: broken, not unknown.
        std::fs::create_dir_all(cfg.data_dir.join("plugins/ecc/skills")).unwrap();
        let err = check_skillsets(&cfg, ["ecc"]).unwrap_err();
        assert!(!err.contains("unknown skillset"), "{err}");
        assert!(err.starts_with("skillset \"ecc\" is invalid: "), "{err}");
        assert!(err.contains("missing plugin manifest"), "names the rule: {err}");
        assert!(err.contains("plugin.json"), "names the file: {err}");

        // A manifest that does not parse is named by its path.
        std::fs::write(cfg.data_dir.join("plugins/ecc/plugin.json"), "{broken").unwrap();
        let err = check_skillsets(&cfg, ["ecc"]).unwrap_err();
        assert!(err.contains("plugins/ecc/plugin.json: invalid plugin manifest"), "{err}");

        // Fixed, it passes, and a name that is truly absent is still "unknown".
        std::fs::write(cfg.data_dir.join("plugins/ecc/plugin.json"), r#"{"name": "ecc"}"#).unwrap();
        assert_eq!(check_skillsets(&cfg, ["ecc"]), Ok(()));
        assert_eq!(
            check_skillsets(&cfg, ["ecc", "ec"]).unwrap_err(),
            "unknown skillset \"ec\"; available: ecc"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_listing_merges_both_places_and_counts_what_claude_code_loads() {
        let (root, cfg) = install();
        let app_plugins = cfg.assets.clone().unwrap().join("plugins");
        plugin(&app_plugins.join("ecc"), "2.2.1", &["tdd", "debugging"], &["planner"]);
        plugin(&app_plugins.join("superpowers"), "6.3.0", &["brainstorming"], &[]);
        plugin(&cfg.data_dir.join("plugins/superpowers"), "6.4.0-local", &[], &[]);
        plugin(&cfg.data_dir.join("plugins/team-skills"), "1.0.0", &["house-style"], &[]);
        std::fs::write(cfg.data_dir.join("plugins/stray-file"), "").unwrap();

        let listing = available(&cfg);
        let plugins = listing["plugins"].as_array().unwrap();
        let summary: Vec<(String, String, bool, u64, u64)> = plugins
            .iter()
            .map(|p| {
                (
                    p["name"].as_str().unwrap().into(),
                    p["source"].as_str().unwrap().into(),
                    p["shadows_vendored"].as_bool().unwrap(),
                    p["skills"].as_u64().unwrap(),
                    p["agents"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("ecc".into(), "vendored".into(), false, 2, 1),
                ("superpowers".into(), "local".into(), true, 0, 0),
                ("team-skills".into(), "local".into(), false, 1, 0),
            ]
        );
        assert_eq!(plugins[1]["version"], "6.4.0-local");
        assert_eq!(listing["local_root"], json!(cfg.data_dir.join("plugins")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_app_or_plugins_folder_lists_nothing() {
        let root = std::env::temp_dir().join(format!("colonizer-plugins-test-{}", crate::util::short_id()));
        assert_eq!(available(&settings(&root, None))["plugins"], json!([]));
        assert_eq!(
            available(&settings(&root, Some(root.join("no-such-app"))))["plugins"],
            json!([])
        );
    }
}
