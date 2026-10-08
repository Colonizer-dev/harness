//! The built-in "TypeScript: remove `any`" loop: on a schedule, count the explicit `any` in every
//! TypeScript repository the operator opted in, and hand one small batch per repository to a colony
//! that replaces them with real types.
//!
//! **Off by default, and opt-in per repository or org.** Nothing runs until the loop is switched on
//! *and* its allowlist names an org (`acme`) or a repository (`acme/app`); both start empty.
//!
//! **The count is deterministic and spends no model tokens.** It runs on the mothership, never in a
//! colony, over the TypeScript sources at the default branch of the mothership's bare mirror. When
//! the repository's own `typescript` package can be had — already in `node_modules`, or installed
//! from the repository's lockfile by its package manager *offline*, from the host's cache — a real
//! parse with its compiler API finds every `any` keyword, and (when asked) a program built with
//! `noImplicitAny` counts the implicit ones. Otherwise a token scan that skips comments, strings,
//! template text and regular expressions counts the same forms, and the report says which method
//! was used and why. Nothing here downloads a package or a tool.
//!
//! **The fix is one small batch per repository per run.** The module (directory) with the most
//! explicit `any` — unless a colony is already on it — goes to one colony, capped at 20 occurrences
//! by default, with a brief that lists each `file:line` and forbids the shortcuts: no `as` casts or
//! `@ts-ignore`/`@ts-expect-error`/`eslint-disable` to silence the compiler, no new `any`, no change
//! in behaviour, and the repository's type check and tests must pass. A per-repository cooldown, a
//! per-run cap and `COLONIZER_NO_EXTERNAL_EFFECTS` hold dispatches back.
//!
//! **The outcome is checked.** Once a batch colony's pull request is published, its branch is
//! counted again; when the module's count did not drop, or suppressions, casts or `any` elsewhere
//! were added, the report and an attention item say so.
//!
//! **Every run is reported** — totals, the busiest modules, the trend against earlier runs, what was
//! dispatched and what was skipped — in the loop's history, the activity log and the cockpit. A dry
//! run does all of the reading and none of the writing.

use crate::{
    ApiResult, App, Shared,
    activity::Entry,
    authority, client_error,
    schedule::{Cadence, next_run_after},
    sessions::{self, NewSession, Session, SessionStatus},
    util::{short_id, valid_repo, write_atomic},
};
use anyhow::{Context, Result};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};

/// The loop's name, as the cockpit and the activity log show it.
pub const NAME: &str = "TypeScript: remove any";
/// The origin tag a batch colony carries: `ts-any:<module>`.
pub const ORIGIN_PREFIX: &str = "ts-any:";
/// The title every batch colony starts with, followed by its module.
const TITLE_PREFIX: &str = "TypeScript: remove any in ";
const FILE: &str = "ts-any-loop.json";
/// Runs kept in the loop's history.
const HISTORY: usize = 30;
/// Totals kept per repository for the trend.
const TREND: usize = 60;
/// Files and modules kept per repository in a stored report.
const MAX_STORED_FILES: usize = 100;
const TOP_MODULES: usize = 10;
/// The most frequent the loop may run: hourly.
pub const MIN_INTERVAL_MINUTES: u32 = 60;
/// How long the TypeScript count may run on one checkout, and an offline install may take.
const TOOL_LIMIT: Duration = Duration::from_secs(10 * 60);
const INSTALL_LIMIT: Duration = Duration::from_secs(5 * 60);
/// Source files larger than this are not read (generated bundles, not code anyone types).
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// A dispatch record is forgotten this long after it was made, once its colony is closed.
const RECORD_DAYS: i64 = 30;
/// Post-checks per run: each is a checkout and a count, so a run never does many.
const MAX_CHECKS_PER_RUN: usize = 3;

// --- the token scan -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokKind {
    Ident,
    Punct,
    Literal,
}

#[derive(Clone, Debug)]
struct Tok {
    kind: TokKind,
    text: String,
    line: u32,
    col: u32,
}

struct Lexer {
    c: Vec<char>,
    i: usize,
    line: u32,
    col: u32,
}

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek(0)?;
        self.i += 1;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    /// Template text after a backtick, or after the `}` that closed a `${`: true when it stopped
    /// at a `${` (code follows), false at the closing backtick or the end.
    fn template(&mut self) -> bool {
        while let Some(ch) = self.bump() {
            match ch {
                '\\' => {
                    self.bump();
                }
                '`' => return false,
                '$' if self.peek(0) == Some('{') => {
                    self.bump();
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// The length of a regular-expression literal starting here, if one fits on the line.
    fn regex_len(&self) -> Option<usize> {
        let mut j = self.i + 1;
        let mut class = false;
        loop {
            match *self.c.get(j)? {
                '\n' | '\r' => return None,
                '\\' => j += 1,
                '[' => class = true,
                ']' => class = false,
                '/' if !class => break,
                _ => {}
            }
            j += 1;
        }
        j += 1;
        while self.c.get(j).is_some_and(char::is_ascii_alphabetic) {
            j += 1;
        }
        Some(j - self.i)
    }
}

/// Keywords after which an expression starts (so `<` opens an assertion and `/` a regex).
const EXPR_KEYWORDS: &[&str] = &[
    "return",
    "yield",
    "await",
    "typeof",
    "case",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "else",
    "do",
    "instanceof",
];

fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(t) => match t.kind {
            TokKind::Literal => false,
            TokKind::Ident => EXPR_KEYWORDS.contains(&t.text.as_str()),
            // `<` then `/` is a JSX closing tag, not a regex.
            TokKind::Punct => !matches!(t.text.as_str(), ")" | "]" | "}" | "<"),
        },
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The code tokens of a TypeScript source (strings, template text, regexes and numbers become one
/// opaque literal each) and its comments, with their lines.
fn lex(src: &str) -> (Vec<Tok>, Vec<(u32, String)>) {
    let mut lx = Lexer {
        c: src.chars().collect(),
        i: 0,
        line: 1,
        col: 1,
    };
    let mut toks: Vec<Tok> = Vec::new();
    let mut comments = Vec::new();
    let mut braces = 0usize;
    // The brace depth each open `${` returns to.
    let mut templates: Vec<usize> = Vec::new();
    while let Some(ch) = lx.peek(0) {
        let (line, col) = (lx.line, lx.col);
        let push = |toks: &mut Vec<Tok>, kind: TokKind, text: &str| {
            toks.push(Tok {
                kind,
                text: text.to_string(),
                line,
                col,
            })
        };
        if ch.is_whitespace() {
            lx.bump();
            continue;
        }
        if ch == '/' && lx.peek(1) == Some('/') {
            let mut text = String::new();
            while let Some(c) = lx.peek(0) {
                if c == '\n' {
                    break;
                }
                text.push(c);
                lx.bump();
            }
            comments.push((line, text));
            continue;
        }
        if ch == '/' && lx.peek(1) == Some('*') {
            let mut text = String::new();
            lx.bump();
            lx.bump();
            while let Some(c) = lx.peek(0) {
                if c == '*' && lx.peek(1) == Some('/') {
                    lx.bump();
                    lx.bump();
                    break;
                }
                text.push(c);
                lx.bump();
            }
            comments.push((line, text));
            continue;
        }
        if ch == '\'' || ch == '"' {
            lx.bump();
            while let Some(c) = lx.peek(0) {
                if c == '\n' {
                    break;
                }
                lx.bump();
                if c == '\\' {
                    lx.bump();
                } else if c == ch {
                    break;
                }
            }
            push(&mut toks, TokKind::Literal, "\"\"");
            continue;
        }
        if ch == '`' {
            lx.bump();
            if lx.template() {
                templates.push(braces);
                braces += 1;
                push(&mut toks, TokKind::Punct, "{");
            } else {
                push(&mut toks, TokKind::Literal, "``");
            }
            continue;
        }
        if ch == '}' {
            lx.bump();
            braces = braces.saturating_sub(1);
            push(&mut toks, TokKind::Punct, "}");
            if templates.last() == Some(&braces) {
                templates.pop();
                if lx.template() {
                    templates.push(braces);
                    braces += 1;
                    push(&mut toks, TokKind::Punct, "{");
                } else {
                    push(&mut toks, TokKind::Literal, "``");
                }
            }
            continue;
        }
        if ch == '{' {
            lx.bump();
            braces += 1;
            push(&mut toks, TokKind::Punct, "{");
            continue;
        }
        if ch == '/'
            && regex_allowed(toks.last())
            && let Some(len) = lx.regex_len()
        {
            for _ in 0..len {
                lx.bump();
            }
            push(&mut toks, TokKind::Literal, "/re/");
            continue;
        }
        if is_ident_start(ch) {
            let mut text = String::new();
            while let Some(c) = lx.peek(0).filter(|c| is_ident_part(*c)) {
                text.push(c);
                lx.bump();
            }
            push(&mut toks, TokKind::Ident, &text);
            continue;
        }
        if ch.is_ascii_digit() {
            while lx.peek(0).is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_') {
                lx.bump();
            }
            push(&mut toks, TokKind::Literal, "0");
            continue;
        }
        let three: String = (0..3).filter_map(|k| lx.peek(k)).collect();
        let op = ["===", "!==", "...", "=>", "==", "!=", "||", "&&", "??", "?."]
            .into_iter()
            .find(|op| three.starts_with(op))
            .filter(|op| *op != "?." || !lx.peek(2).is_some_and(|c| c.is_ascii_digit()));
        let text = op.map(str::to_string).unwrap_or_else(|| ch.to_string());
        for _ in 0..text.chars().count() {
            lx.bump();
        }
        push(&mut toks, TokKind::Punct, &text);
    }
    (toks, comments)
}

/// The kind of explicit `any`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    /// `x: any`, a parameter, property or return type.
    Annotation,
    /// `x as any`.
    As,
    /// `<any>x`.
    Angle,
    /// `Foo<any>`, `f<any>()`.
    TypeArgument,
    /// `any[]`.
    Array,
    /// `Array<any>`, `ReadonlyArray<any>`.
    ArrayGeneric,
    /// `Record<string, any>`.
    Record,
    /// `<T = any>`.
    GenericDefault,
    /// Anywhere else a type goes: `string | any`, `() => any`, `type X = any`, a tuple.
    Other,
}

impl Form {
    pub fn label(self) -> &'static str {
        match self {
            Form::Annotation => ": any",
            Form::As => "as any",
            Form::Angle => "<any>",
            Form::TypeArgument => "Foo<any>",
            Form::Array => "any[]",
            Form::ArrayGeneric => "Array<any>",
            Form::Record => "Record<…, any>",
            Form::GenericDefault => "<T = any>",
            Form::Other => "any",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Form::Annotation => "annotation",
            Form::As => "as",
            Form::Angle => "angle",
            Form::TypeArgument => "type_argument",
            Form::Array => "array",
            Form::ArrayGeneric => "array_generic",
            Form::Record => "record",
            Form::GenericDefault => "generic_default",
            Form::Other => "other",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        [
            Form::Annotation,
            Form::As,
            Form::Angle,
            Form::TypeArgument,
            Form::Array,
            Form::ArrayGeneric,
            Form::Record,
            Form::GenericDefault,
            Form::Other,
        ]
        .into_iter()
        .find(|f| f.key() == key)
    }
}

/// The unmatched opener (`(`, `[`, `{` or `<`) a token sits inside, looking back within its
/// statement.
fn enclosing(t: &[Tok], k: usize) -> Option<usize> {
    let (mut paren, mut square, mut curly, mut angle) = (0i32, 0i32, 0i32, 0i32);
    for j in (k.saturating_sub(256)..k).rev() {
        if t[j].kind != TokKind::Punct {
            continue;
        }
        match t[j].text.as_str() {
            ")" => paren += 1,
            "]" => square += 1,
            "}" => curly += 1,
            ">" => angle += 1,
            "(" if paren == 0 => return Some(j),
            "[" if square == 0 => return Some(j),
            "{" if curly == 0 => return Some(j),
            "<" if angle == 0 => return Some(j),
            "(" => paren -= 1,
            "[" => square -= 1,
            "{" => curly -= 1,
            "<" => angle -= 1,
            ";" if paren + square + curly + angle == 0 => return None,
            _ => {}
        }
    }
    None
}

/// Commas at the top level between an opener and a token.
fn top_level_commas(t: &[Tok], open: usize, k: usize) -> usize {
    let mut depth = 0i32;
    let mut commas = 0;
    for tok in &t[open + 1..k] {
        match tok.text.as_str() {
            "(" | "[" | "{" | "<" => depth += 1,
            ")" | "]" | "}" | ">" => depth -= 1,
            "," if depth == 0 => commas += 1,
            _ => {}
        }
    }
    commas
}

/// Whether the `=` at `eq` is a type alias's: `type Name = …`, `export type Name<T> = …`.
fn type_alias_rhs(t: &[Tok], eq: usize) -> bool {
    for j in (eq.saturating_sub(64)..eq).rev() {
        match t[j].text.as_str() {
            ";" | "{" | "}" => return false,
            "type" if t[j].kind == TokKind::Ident => {
                return j == 0 || matches!(t[j - 1].text.as_str(), ";" | "{" | "}" | "export" | "declare");
            }
            _ => {}
        }
    }
    false
}

/// Which explicit `any` the `any` token at `k` is, or `None` when it is not one (an identifier
/// named `any`, a property, a call).
fn classify(t: &[Tok], k: usize) -> Option<Form> {
    let text = |j: usize| t.get(j).map(|x| x.text.as_str()).unwrap_or("");
    let prev = if k > 0 { text(k - 1) } else { "" };
    let next = text(k + 1);
    if matches!(prev, "." | "?.") || matches!(next, "(" | "." | "?.") {
        return None;
    }
    if next == "[" && text(k + 2) == "]" {
        return Some(Form::Array);
    }
    match prev {
        "as" => return Some(Form::As),
        ":" => return Some(Form::Annotation),
        "|" | "&" | "=>" | "extends" | "keyof" | "readonly" | "is" => return Some(Form::Other),
        _ => {}
    }
    if prev == "=" {
        return match enclosing(t, k).map(text) {
            Some("<") => Some(Form::GenericDefault),
            Some(_) => None,
            None => type_alias_rhs(t, k - 1).then_some(Form::Other),
        };
    }
    if matches!(prev, "<" | "," | "[") {
        let open = enclosing(t, k)?;
        let before = open.checked_sub(1).and_then(|j| t.get(j));
        let name = before.map(|b| b.text.as_str()).unwrap_or("");
        return match text(open) {
            "<" => {
                if name == "Record" && top_level_commas(t, open, k) == 1 {
                    Some(Form::Record)
                } else if matches!(name, "Array" | "ReadonlyArray") {
                    Some(Form::ArrayGeneric)
                } else if before
                    .is_some_and(|b| (b.kind == TokKind::Ident && !EXPR_KEYWORDS.contains(&b.text.as_str())) || b.text == ">")
                {
                    Some(Form::TypeArgument)
                } else {
                    Some(Form::Angle)
                }
            }
            // A tuple type: `[string, any]` after `:`, `=>`, `|`, `&` or `<`.
            "[" => matches!(name, ":" | "=>" | "|" | "&" | "<" | "readonly").then_some(Form::Other),
            _ => None,
        };
    }
    None
}

/// Which tokens are inside an import or export clause (`import { a as b }`, `export * as ns`),
/// where `as` renames rather than casts.
fn module_clauses(t: &[Tok]) -> Vec<bool> {
    let mut clause = vec![false; t.len()];
    for k in 0..t.len() {
        if t[k].kind != TokKind::Ident
            || !matches!(t[k].text.as_str(), "import" | "export")
            || (k > 0 && matches!(t[k - 1].text.as_str(), "." | "?."))
        {
            continue;
        }
        let mut j = k + 1;
        if t.get(j).is_some_and(|x| x.text == "type") {
            j += 1;
        }
        if t.get(j).is_some_and(|x| x.kind == TokKind::Ident) && t.get(j + 1).is_some_and(|x| x.text == ",") {
            j += 2;
        }
        match t.get(j).map(|x| x.text.as_str()) {
            Some("{") => {
                let mut depth = 0;
                for m in j..t.len() {
                    match t[m].text.as_str() {
                        "{" => depth += 1,
                        "}" => {
                            depth -= 1;
                            if depth == 0 {
                                clause[k..=m].iter_mut().for_each(|c| *c = true);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("*") => {
                let end = (j + 2).min(t.len() - 1);
                clause[k..=end].iter_mut().for_each(|c| *c = true);
            }
            _ => {}
        }
    }
    clause
}

/// Whether a comment switches a check off.
pub fn is_suppression(comment: &str) -> bool {
    ["@ts-ignore", "@ts-expect-error", "@ts-nocheck", "eslint-disable"]
        .iter()
        .any(|s| comment.contains(s))
}

/// What the token scan finds in one source file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileScan {
    /// Every explicit `any`: line, column (1-based), form.
    pub explicit: Vec<(u32, u32, Form)>,
    /// `as` type assertions other than `as const` and `as any`.
    pub as_casts: usize,
    /// `@ts-ignore`, `@ts-expect-error`, `@ts-nocheck` and `eslint-disable` comments.
    pub suppressions: usize,
}

/// Counts one TypeScript source with the token scan. Pure.
pub fn scan_source(src: &str) -> FileScan {
    let (t, comments) = lex(src);
    let clause = module_clauses(&t);
    let mut scan = FileScan {
        suppressions: comments.iter().filter(|(_, c)| is_suppression(c)).count(),
        ..FileScan::default()
    };
    let text = |j: usize| t.get(j).map(|x| x.text.as_str()).unwrap_or("");
    for (k, tok) in t.iter().enumerate() {
        if tok.kind != TokKind::Ident || clause[k] {
            continue;
        }
        let prev = if k > 0 { text(k - 1) } else { "" };
        if tok.text == "any" {
            if let Some(form) = classify(&t, k) {
                scan.explicit.push((tok.line, tok.col, form));
            }
        } else if tok.text == "as"
            && !matches!(prev, "." | "?." | "*" | "const" | "let" | "var" | "function" | "{" | ",")
            && !matches!(text(k + 1), "const" | "any" | ":" | "=" | "," | ")" | ";" | "}" | "")
        {
            scan.as_casts += 1;
        }
    }
    scan
}

// --- files and modules --------------------------------------------------------------------------

/// A TypeScript source: `.ts`, `.tsx`, `.mts`, `.cts` (declaration files included).
pub fn is_ts_source(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|ext| file.ends_with(ext))
}

pub fn is_tsconfig(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file == "tsconfig.json" || (file.starts_with("tsconfig.") && file.ends_with(".json"))
}

/// Directories whose sources nobody types by hand: dependencies, build output, vendored code.
fn not_counted(path: &str) -> bool {
    path.split('/').any(|p| {
        matches!(
            p,
            "node_modules"
                | "dist"
                | "build"
                | "out"
                | "coverage"
                | ".next"
                | ".nuxt"
                | ".turbo"
                | ".svelte-kit"
                | "vendor"
                | ".git"
        )
    })
}

/// The module a file belongs to: its directory, `.` at the root.
pub fn module_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_else(|| ".".to_string())
}

/// The files the count reads, in order.
pub fn counted_files(files: &[String]) -> Vec<String> {
    let mut out: Vec<String> = files.iter().filter(|p| is_ts_source(p) && !not_counted(p)).cloned().collect();
    out.sort();
    out
}

// --- the repository's own TypeScript ------------------------------------------------------------

/// What the compiler-API count printed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TscOutput {
    pub version: String,
    /// `(file, line, col, form)`.
    pub occurrences: Vec<(String, u32, u32, Form)>,
    /// Implicit `any` per file, when they were asked for.
    pub implicit: Option<BTreeMap<String, usize>>,
    pub errors: Vec<String>,
}

/// Reads the script's JSON answer.
pub fn parse_tsc_output(text: &str) -> Result<TscOutput> {
    let start = text.find('{').context("the TypeScript count printed no JSON")?;
    let v: Value = serde_json::from_str(text[start..].trim()).context("the TypeScript count printed JSON this cannot read")?;
    let version = v["version"]
        .as_str()
        .context("no TypeScript version in the answer")?
        .to_string();
    let mut out = TscOutput {
        version,
        ..TscOutput::default()
    };
    for o in v["occurrences"].as_array().into_iter().flatten() {
        let (Some(file), Some(line), Some(col)) = (o["file"].as_str(), o["line"].as_u64(), o["col"].as_u64()) else {
            continue;
        };
        let form = o["form"].as_str().and_then(Form::from_key).unwrap_or(Form::Other);
        out.occurrences.push((file.to_string(), line as u32, col as u32, form));
    }
    if let Some(map) = v["implicit"].as_object() {
        out.implicit = Some(
            map.iter()
                .filter_map(|(k, n)| n.as_u64().map(|n| (k.clone(), n as usize)))
                .collect(),
        );
    }
    out.errors = v["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str().map(str::to_string))
        .collect();
    Ok(out)
}

/// The count with the compiler API, run by `node` with the repository's own `typescript`:
/// `node script <typescript.js> <root> <list.json> <implicit 0|1>`, where the list names the
/// files to count and the tsconfigs to build for implicit `any`. Prints one JSON object.
const TSC_SCRIPT: &str = r#"'use strict';
const [, , tsPath, root, listPath, implicit] = process.argv;
const ts = require(tsPath);
const fs = require('fs');
const path = require('path');
const list = JSON.parse(fs.readFileSync(listPath, 'utf8'));
const out = { version: ts.version, occurrences: [], implicit: null, errors: [] };
function form(node) {
  const p = node.parent;
  if (!p) return 'other';
  if (ts.isArrayTypeNode(p)) return 'array';
  if (ts.isAsExpression(p) && p.type === node) return 'as';
  if (ts.isTypeAssertionExpression && ts.isTypeAssertionExpression(p)) return 'angle';
  if (ts.isTypeParameterDeclaration(p) && p.default === node) return 'generic_default';
  if (ts.isTypeReferenceNode(p) && p.typeArguments && p.typeArguments.includes(node)) {
    const name = p.typeName.getText();
    if (name === 'Array' || name === 'ReadonlyArray') return 'array_generic';
    if (name === 'Record' && p.typeArguments.indexOf(node) === 1) return 'record';
    return 'type_argument';
  }
  if (p.typeArguments && p.typeArguments.includes(node)) return 'type_argument';
  if (ts.isFunctionTypeNode(p) || ts.isConstructorTypeNode(p) || ts.isTypeAliasDeclaration(p)) return 'other';
  if (p.type === node) return 'annotation';
  return 'other';
}
for (const file of list.files) {
  let text;
  try { text = fs.readFileSync(path.join(root, file), 'utf8'); } catch (e) { out.errors.push(file + ': ' + e.message); continue; }
  const kind = file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, kind);
  const visit = (n) => {
    if (n.kind === ts.SyntaxKind.AnyKeyword) {
      const at = sf.getLineAndCharacterOfPosition(n.getStart(sf));
      out.occurrences.push({ file, line: at.line + 1, col: at.character + 1, form: form(n) });
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
}
if (implicit === '1') {
  const codes = new Set([7005, 7006, 7008, 7010, 7011, 7015, 7016, 7017, 7018, 7019, 7022, 7023, 7024, 7025, 7031, 7032, 7033, 7034, 7053]);
  const wanted = new Set(list.files);
  const seen = new Set();
  const counts = {};
  for (const cfgPath of list.tsconfigs) {
    try {
      const abs = path.join(root, cfgPath);
      const cfg = ts.readConfigFile(abs, ts.sys.readFile);
      if (cfg.error) { out.errors.push(cfgPath + ': ' + ts.flattenDiagnosticMessageText(cfg.error.messageText, ' ')); continue; }
      const parsed = ts.parseJsonConfigFileContent(cfg.config, ts.sys, path.dirname(abs));
      const options = Object.assign({}, parsed.options, { noImplicitAny: true, noEmit: true, incremental: false, composite: false });
      const program = ts.createProgram({ rootNames: parsed.fileNames, options });
      for (const d of ts.getPreEmitDiagnostics(program)) {
        if (!codes.has(d.code) || !d.file) continue;
        const rel = path.relative(root, d.file.fileName).split(path.sep).join('/');
        const key = rel + ':' + d.start + ':' + d.code;
        if (!wanted.has(rel) || seen.has(key)) continue;
        seen.add(key);
        counts[rel] = (counts[rel] || 0) + 1;
      }
    } catch (e) { out.errors.push(cfgPath + ': ' + e.message); }
  }
  out.implicit = counts;
}
process.stdout.write(JSON.stringify(out));
"#;

// --- a measurement ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// A real parse with the repository's own `typescript`.
    Typescript,
    /// The token scan.
    TokenScan,
}

/// One explicit `any`, where it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Occurrence {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub form: Form,
    /// The source line, trimmed, for the brief.
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileCount {
    pub path: String,
    pub module: String,
    pub explicit: usize,
    #[serde(default)]
    pub implicit: Option<usize>,
    #[serde(default)]
    pub as_casts: usize,
    #[serde(default)]
    pub suppressions: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModuleCount {
    pub module: String,
    pub explicit: usize,
    /// Files in it with at least one explicit `any`.
    pub files: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    pub method: Method,
    /// Which method, and why: "the repository's TypeScript 5.6.3 (compiler API)" or why the token
    /// scan was used instead.
    pub method_note: String,
    pub ts_version: Option<String>,
    pub total: usize,
    pub implicit: Option<usize>,
    pub as_casts: usize,
    pub suppressions: usize,
    pub ts_files: usize,
    pub forms: BTreeMap<String, usize>,
    /// Every counted file with something to count, the most `any` first.
    pub files: Vec<FileCount>,
    /// Every module with an explicit `any`, the most first.
    pub modules: Vec<ModuleCount>,
    pub occurrences: Vec<Occurrence>,
    pub notes: Vec<String>,
}

/// Counts the sources under `root`. With `tsc`, the explicit `any` come from the compiler's parse;
/// without, from the token scan. Casts and suppression comments always come from the token scan.
pub fn measure(root: &FsPath, files: &[String], tsc: Option<&TscOutput>, method_note: String) -> Measurement {
    let mut by_file: BTreeMap<&str, Vec<(u32, u32, Form)>> = BTreeMap::new();
    if let Some(tsc) = tsc {
        for (file, line, col, form) in &tsc.occurrences {
            by_file.entry(file.as_str()).or_default().push((*line, *col, *form));
        }
    }
    let mut m = Measurement {
        method: if tsc.is_some() {
            Method::Typescript
        } else {
            Method::TokenScan
        },
        method_note,
        ts_version: tsc.map(|t| t.version.clone()),
        total: 0,
        implicit: tsc.and_then(|t| t.implicit.as_ref()).map(|i| i.values().sum()),
        as_casts: 0,
        suppressions: 0,
        ts_files: 0,
        forms: BTreeMap::new(),
        files: Vec::new(),
        modules: Vec::new(),
        occurrences: Vec::new(),
        notes: tsc.map(|t| t.errors.iter().take(5).cloned().collect()).unwrap_or_default(),
    };
    let mut modules: BTreeMap<String, ModuleCount> = BTreeMap::new();
    for path in counted_files(files) {
        let file = root.join(&path);
        if std::fs::metadata(&file).map(|md| md.len() > MAX_FILE_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else { continue };
        m.ts_files += 1;
        let scan = scan_source(&src);
        let mut found = match tsc {
            Some(_) => by_file.remove(path.as_str()).unwrap_or_default(),
            None => scan.explicit.clone(),
        };
        found.sort();
        let lines: Vec<&str> = src.lines().collect();
        let module = module_of(&path);
        for (line, col, form) in &found {
            *m.forms.entry(form.key().to_string()).or_default() += 1;
            let text = lines
                .get(line.saturating_sub(1) as usize)
                .map(|l| l.trim().chars().take(160).collect())
                .unwrap_or_default();
            m.occurrences.push(Occurrence {
                file: path.clone(),
                line: *line,
                col: *col,
                form: *form,
                text,
            });
        }
        let count = FileCount {
            path: path.clone(),
            module: module.clone(),
            explicit: found.len(),
            implicit: tsc
                .and_then(|t| t.implicit.as_ref())
                .map(|i| i.get(&path).copied().unwrap_or(0)),
            as_casts: scan.as_casts,
            suppressions: scan.suppressions,
        };
        m.total += count.explicit;
        m.as_casts += count.as_casts;
        m.suppressions += count.suppressions;
        if count.explicit > 0 {
            let e = modules.entry(module.clone()).or_insert_with(|| ModuleCount {
                module,
                ..ModuleCount::default()
            });
            e.explicit += count.explicit;
            e.files += 1;
        }
        if count.explicit + count.implicit.unwrap_or(0) + count.as_casts + count.suppressions > 0 {
            m.files.push(count);
        }
    }
    m.files.sort_by(|a, b| b.explicit.cmp(&a.explicit).then(a.path.cmp(&b.path)));
    m.modules = modules.into_values().collect();
    m.modules
        .sort_by(|a, b| b.explicit.cmp(&a.explicit).then(a.module.cmp(&b.module)));
    m
}

/// What a module and the repository held at one commit: what a post-check compares.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub module_explicit: usize,
    pub module_as_casts: usize,
    pub module_suppressions: usize,
    pub total: usize,
    pub as_casts: usize,
    pub suppressions: usize,
}

impl Baseline {
    pub fn of(m: &Measurement, module: &str) -> Self {
        let in_module = m.files.iter().filter(|f| f.module == module);
        let (mut casts, mut sup) = (0, 0);
        for f in in_module {
            casts += f.as_casts;
            sup += f.suppressions;
        }
        Baseline {
            module_explicit: m.modules.iter().find(|x| x.module == module).map_or(0, |x| x.explicit),
            module_as_casts: casts,
            module_suppressions: sup,
            total: m.total,
            as_casts: m.as_casts,
            suppressions: m.suppressions,
        }
    }
}

// --- choosing the batch -------------------------------------------------------------------------

/// One colony's worth of work: a repository's module and the occurrences it is given.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub repo: String,
    pub module: String,
    pub sha: String,
    /// The explicit `any` in the module at `sha`.
    pub module_total: usize,
    /// The batch: at most the cap, in file and line order.
    pub occurrences: Vec<Occurrence>,
    pub method: Method,
    pub before: Baseline,
}

impl Target {
    pub fn origin(&self) -> String {
        format!("{ORIGIN_PREFIX}{}", self.module)
    }

    pub fn title(&self) -> String {
        format!(
            "{TITLE_PREFIX}{} ({} of {})",
            self.module,
            self.occurrences.len(),
            self.module_total
        )
    }
}

/// Whether a colony is still working on its target: live, queued, parked, publishing, or its pull
/// request is open.
fn still_open(s: &Session) -> bool {
    matches!(s.status, SessionStatus::Queued | SessionStatus::Blocked)
        || s.status == SessionStatus::Parked
        || s.status == SessionStatus::PrOpened
        || s.status.busy()
}

/// The colony that already targets this module — a batch colony (by origin, or by the title a
/// hand-started one would carry) on the same repository that is live or has its pull request open.
pub fn duplicate_of(repo: &str, module: &str, sessions: &[Session]) -> Option<String> {
    let origin = format!("{ORIGIN_PREFIX}{module}");
    let title = format!("{TITLE_PREFIX}{module}");
    sessions
        .iter()
        .filter(|s| s.repo.eq_ignore_ascii_case(repo) && still_open(s))
        .find(|s| {
            s.origin.as_deref() == Some(origin.as_str())
                || s.issue_title == title
                || s.issue_title.starts_with(&format!("{title} ("))
        })
        .map(|s| s.id.clone())
}

/// A repository or module the run did not dispatch, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub repo: String,
    #[serde(default)]
    pub module: Option<String>,
    pub reason: String,
}

/// Picks one repository's batch: the module with the most explicit `any` that no open colony
/// already targets, and its first `cap` occurrences in file and line order. Pure.
pub fn select(repo: &str, sha: &str, m: &Measurement, cap: usize, sessions: &[Session]) -> (Option<Target>, Vec<Skipped>) {
    let mut skipped = Vec::new();
    for module in m.modules.iter().filter(|x| x.explicit > 0) {
        if let Some(session) = duplicate_of(repo, &module.module, sessions) {
            skipped.push(Skipped {
                repo: repo.to_string(),
                module: Some(module.module.clone()),
                reason: format!("colony {session} already targets this module (it is live or its pull request is open)"),
            });
            continue;
        }
        let mut occurrences: Vec<Occurrence> = m
            .occurrences
            .iter()
            .filter(|o| module_of(&o.file) == module.module)
            .cloned()
            .collect();
        occurrences.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)).then(a.col.cmp(&b.col)));
        occurrences.truncate(cap.max(1));
        return (
            Some(Target {
                repo: repo.to_string(),
                module: module.module.clone(),
                sha: sha.to_string(),
                module_total: module.explicit,
                occurrences,
                method: m.method,
                before: Baseline::of(m, &module.module),
            }),
            skipped,
        );
    }
    (None, skipped)
}

/// What a run dispatches and what it holds back: the kill switch first, then the cooldown and the
/// per-run cap. One target per repository at most, by construction. Pure.
pub fn plan_dispatch(
    targets: Vec<Target>,
    settings: &Settings,
    cooldowns: &BTreeMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
    blocked: bool,
) -> (Vec<Target>, Vec<Skipped>) {
    let mut go: Vec<Target> = Vec::new();
    let mut skipped = Vec::new();
    for t in targets {
        let skip = |reason: String| Skipped {
            repo: t.repo.clone(),
            module: Some(t.module.clone()),
            reason,
        };
        if blocked {
            skipped.push(skip(
                "external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS): reported only, no colony started".to_string(),
            ));
            continue;
        }
        if let Some(last) = cooldowns.get(&t.repo.to_ascii_lowercase()) {
            let until = *last + ChronoDuration::hours(i64::from(settings.cooldown_hours));
            if until > now {
                skipped.push(skip(format!(
                    "the repository is cooling down until {} after the last dispatch",
                    until.format("%Y-%m-%d %H:%M UTC")
                )));
                continue;
            }
        }
        if go.iter().any(|g| g.repo.eq_ignore_ascii_case(&t.repo)) {
            skipped.push(skip("one batch per repository per run".to_string()));
            continue;
        }
        if go.len() as u32 >= settings.max_per_run {
            skipped.push(skip(format!(
                "at most {} dispatch{} per run",
                settings.max_per_run,
                if settings.max_per_run == 1 { "" } else { "es" }
            )));
            continue;
        }
        go.push(t);
    }
    (go, skipped)
}

/// What a batch colony is told.
pub fn brief(t: &Target) -> String {
    let method = match t.method {
        Method::Typescript => "a parse with the repository's own TypeScript",
        Method::TokenScan => "a token scan",
    };
    let short = t.sha.get(..7).unwrap_or(&t.sha);
    let mut lines = vec![
        format!(
            "Remove explicit `any` from {} in {}. The mothership's scheduled TypeScript check ({method}) counted {} explicit `any` in this directory at {short}; this batch is these {}:",
            t.module,
            t.repo,
            t.module_total,
            t.occurrences.len()
        ),
        String::new(),
    ];
    for o in &t.occurrences {
        let code = if o.text.is_empty() {
            String::new()
        } else {
            format!(" — `{}`", o.text.replace('`', "'"))
        };
        lines.push(format!("- {}:{}:{} ({}){code}", o.file, o.line, o.col, o.form.label()));
    }
    lines.extend([
        String::new(),
        "Rules:".to_string(),
        "- Replace each listed `any` with a real type, with `unknown` plus narrowing (type guards, `typeof`, `instanceof`, or a validator the repository already uses), or with a generic.".to_string(),
        "- NEVER silence the compiler instead: no `as` casts to make an error go away (no `as SomeType` on an unchecked value, no `as unknown as T`), no `// @ts-ignore`, `// @ts-expect-error` or `// @ts-nocheck`, no `// eslint-disable` comments, and no new `any` anywhere else.".to_string(),
        "- Don't change runtime behaviour: this is a types-only change. If a real type exposes a bug, leave that `any` and describe the bug in the pull request.".to_string(),
        "- The repository's type check must pass (`tsc --noEmit`, or `tsc -b` for a project-references build; use the repository's own script when it has one), and so must its tests.".to_string(),
        "- Keep the diff small: only the listed occurrences and the lines they need. No refactors, no formatting sweeps, no unrelated files.".to_string(),
        "- If an occurrence cannot be typed without breaking a rule, leave it and say why in the pull request.".to_string(),
        format!(
            "- Title the pull request \"{TITLE_PREFIX}{}\" and list the occurrences it fixes. Do not add any Claude/AI attribution.",
            t.module
        ),
    ]);
    lines.join("\n")
}

// --- the post-check -----------------------------------------------------------------------------

/// A batch colony the loop started, and what it was given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetRecord {
    pub session: String,
    pub repo: String,
    pub module: String,
    /// The default branch's commit the batch was counted at.
    pub sha: String,
    pub at: DateTime<Utc>,
    /// `file:line` of each occurrence in the batch.
    pub occurrences: Vec<String>,
    pub before: Baseline,
    /// The recount once its pull request was published; `None` until then.
    #[serde(default)]
    pub check: Option<PostCheck>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PostCheck {
    pub at: DateTime<Utc>,
    /// The commit counted: the colony's branch head.
    #[serde(default)]
    pub head: Option<String>,
    pub module_before: usize,
    #[serde(default)]
    pub module_after: Option<usize>,
    #[serde(default)]
    pub suppressions_added: i64,
    #[serde(default)]
    pub as_casts_added: i64,
    #[serde(default)]
    pub any_added_elsewhere: i64,
    pub flagged: bool,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl PostCheck {
    pub fn summary(&self) -> String {
        if let Some(note) = &self.note
            && self.module_after.is_none()
        {
            return note.clone();
        }
        let after = self.module_after.map_or("?".to_string(), |n| n.to_string());
        if self.problems.is_empty() {
            format!("any in the module {} → {after}: ok", self.module_before)
        } else {
            format!(
                "any in the module {} → {after}: {}",
                self.module_before,
                self.problems.join("; ")
            )
        }
    }
}

/// Compares a batch's baseline with the count at its pull request's head. Flags a module count that
/// did not drop, and any suppression comment, `as` cast or `any` outside the module that was added.
/// Pure.
pub fn post_check(rec: &TargetRecord, after: &Measurement, head: &str, now: DateTime<Utc>) -> PostCheck {
    let a = Baseline::of(after, &rec.module);
    let b = &rec.before;
    let diff = |x: usize, y: usize| x as i64 - y as i64;
    let suppressions_added = diff(a.module_suppressions, b.module_suppressions).max(diff(a.suppressions, b.suppressions));
    let as_casts_added = diff(a.module_as_casts, b.module_as_casts).max(diff(a.as_casts, b.as_casts));
    let any_added_elsewhere = diff(a.total - a.module_explicit, b.total - b.module_explicit);
    let mut problems = Vec::new();
    if a.module_explicit >= b.module_explicit {
        problems.push(format!(
            "the explicit any in {} did not drop ({} → {})",
            rec.module, b.module_explicit, a.module_explicit
        ));
    }
    if suppressions_added > 0 {
        problems.push(format!(
            "{suppressions_added} suppression comment{} added (@ts-ignore, @ts-expect-error, @ts-nocheck or eslint-disable)",
            if suppressions_added == 1 { "" } else { "s" }
        ));
    }
    if as_casts_added > 0 {
        problems.push(format!(
            "{as_casts_added} `as` cast{} added",
            if as_casts_added == 1 { "" } else { "s" }
        ));
    }
    if any_added_elsewhere > 0 {
        problems.push(format!("{any_added_elsewhere} new any outside {}", rec.module));
    }
    PostCheck {
        at: now,
        head: Some(head.to_string()),
        module_before: b.module_explicit,
        module_after: Some(a.module_explicit),
        suppressions_added,
        as_casts_added,
        any_added_elsewhere,
        flagged: !problems.is_empty(),
        problems,
        note: None,
    }
}

/// A batch whose outcome a person should look at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub repo: String,
    pub module: String,
    pub session: String,
    #[serde(default)]
    pub pr_url: Option<String>,
    pub problems: Vec<String>,
    pub reason: String,
}

fn attention_of(rec: &TargetRecord, pr_url: Option<String>) -> Option<AttentionItem> {
    let check = rec.check.as_ref().filter(|c| c.flagged)?;
    Some(AttentionItem {
        repo: rec.repo.clone(),
        module: rec.module.clone(),
        session: rec.session.clone(),
        pr_url,
        problems: check.problems.clone(),
        reason: format!(
            "the batch for {} in {} needs review before merging: {}",
            rec.module,
            rec.repo,
            check.problems.join("; ")
        ),
    })
}

// --- settings and state -------------------------------------------------------------------------

fn default_cadence() -> Cadence {
    Cadence::Daily { hour: 7, minute: 43 }
}
fn twenty() -> u32 {
    20
}
fn three() -> u32 {
    3
}
fn cooldown() -> u32 {
    20
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The loop's switch. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// The orgs (`acme`) and repositories (`acme/app`) opted in. Empty by default: nothing runs.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Daily by default; as often as hourly.
    #[serde(default = "default_cadence")]
    pub cadence: Cadence,
    /// Occurrences given to one colony.
    #[serde(default = "twenty")]
    pub batch_cap: u32,
    /// Colonies started per run, across every repository (one per repository at most).
    #[serde(default = "three")]
    pub max_per_run: u32,
    /// Hours after a dispatch before the same repository gets another.
    #[serde(default = "cooldown")]
    pub cooldown_hours: u32,
    /// Also count implicit `any` (what `noImplicitAny` flags); only with the repository's TypeScript.
    #[serde(default)]
    pub implicit: bool,
    /// When `node_modules` is absent, install from the lockfile with the repository's package
    /// manager, offline, from the host's cache only.
    #[serde(default = "yes")]
    pub offline_install: bool,
    /// Whether a batch colony publishes its pull request by itself.
    #[serde(default = "yes")]
    pub autopilot: bool,
}

impl Default for Settings {
    fn default() -> Self {
        serde_json::from_value(json!({})).expect("every field has a default")
    }
}

impl Settings {
    /// Refuses what could only fail or burst when the loop fires; tidies the allowlist.
    pub fn validated(mut self) -> Result<Self, String> {
        let mut allow = Vec::new();
        for entry in &self.allow {
            let e = entry.trim().trim_end_matches("/*").to_string();
            if e.is_empty() {
                continue;
            }
            let ok = crate::repo_scope::valid_entry(&e);
            if !ok {
                return Err(format!("{e:?} is not an org or an owner/repo"));
            }
            if !allow.iter().any(|a: &String| a.eq_ignore_ascii_case(&e)) {
                allow.push(e);
            }
        }
        self.allow = allow;
        self.cadence.check()?;
        match self.cadence {
            Cadence::SelfPaced {} => return Err("the TypeScript any loop runs on a fixed cadence, not self-paced".into()),
            Cadence::Interval { minutes } if minutes < MIN_INTERVAL_MINUTES => {
                return Err(format!(
                    "the TypeScript any loop runs at most hourly, got every {minutes} minutes"
                ));
            }
            _ => {}
        }
        if !(1..=100).contains(&self.batch_cap) {
            return Err("batch_cap must be 1 to 100".into());
        }
        if !(1..=10).contains(&self.max_per_run) {
            return Err("max_per_run must be 1 to 10".into());
        }
        if !(1..=24 * 30).contains(&self.cooldown_hours) {
            return Err("cooldown_hours must be 1 to 720".into());
        }
        Ok(self)
    }

    /// Whether the allowlist covers a repository, by its org or by name.
    pub fn covers(&self, repo: &str) -> bool {
        // `*` covers every repository here; a run resolves it first so hidden orgs fall out.
        self.allow.iter().any(|a| crate::repo_scope::entry_matches(a, repo))
    }

    fn active(&self) -> bool {
        self.enabled && !self.allow.is_empty()
    }
}

/// One repository's part of a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoReport {
    pub repo: String,
    #[serde(default)]
    pub sha: Option<String>,
    /// Whether it has a tsconfig; a repository without one is not counted.
    #[serde(default)]
    pub typescript: bool,
    #[serde(default)]
    pub method: Option<Method>,
    #[serde(default)]
    pub method_note: Option<String>,
    #[serde(default)]
    pub ts_version: Option<String>,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub implicit: Option<usize>,
    #[serde(default)]
    pub as_casts: usize,
    #[serde(default)]
    pub suppressions: usize,
    #[serde(default)]
    pub ts_files: usize,
    #[serde(default)]
    pub forms: BTreeMap<String, usize>,
    /// The busiest modules.
    #[serde(default)]
    pub modules: Vec<ModuleCount>,
    /// The busiest files.
    #[serde(default)]
    pub files: Vec<FileCount>,
    /// Earlier real runs' totals, newest first.
    #[serde(default)]
    pub previous: Vec<usize>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl RepoReport {
    /// The change since the last real run.
    pub fn delta(&self) -> Option<i64> {
        self.previous.first().map(|p| self.total as i64 - *p as i64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dispatched {
    pub repo: String,
    pub module: String,
    /// The colony started; `None` in a dry run, which only says what it would start.
    #[serde(default)]
    pub session: Option<String>,
    pub title: String,
    pub occurrences: usize,
    pub module_total: usize,
}

/// A post-check made during a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckLine {
    pub session: String,
    pub repo: String,
    pub module: String,
    #[serde(default)]
    pub pr_url: Option<String>,
    pub flagged: bool,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub dry_run: bool,
    /// `schedule` or `manual`.
    pub trigger: String,
    /// Whether the kill switch held every dispatch.
    #[serde(default)]
    pub blocked: bool,
    pub repos: Vec<RepoReport>,
    /// Explicit `any` across every counted repository.
    pub total: usize,
    pub dispatched: Vec<Dispatched>,
    pub skipped: Vec<Skipped>,
    #[serde(default)]
    pub checks: Vec<CheckLine>,
    /// Post-checks flagged in this run.
    pub attention: Vec<AttentionItem>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Report {
    /// One line for the activity log and the loop's history.
    pub fn summary(&self) -> String {
        let counted = self.repos.iter().filter(|r| r.typescript && r.error.is_none()).count();
        let previous: Option<i64> = {
            let with: Vec<&RepoReport> = self.repos.iter().filter(|r| r.typescript).collect();
            (!with.is_empty() && with.iter().all(|r| !r.previous.is_empty()))
                .then(|| with.iter().map(|r| r.previous[0] as i64).sum())
        };
        let trend = match previous {
            Some(p) if p != self.total as i64 => format!(" ({:+} since the last run)", self.total as i64 - p),
            Some(_) => " (unchanged)".to_string(),
            None => String::new(),
        };
        let verb = if self.dry_run { "would dispatch" } else { "dispatched" };
        let flagged = self.attention.len();
        format!(
            "{counted} TypeScript repositor{}: {} explicit any{trend}; {verb} {}, skipped {}{}",
            if counted == 1 { "y" } else { "ies" },
            self.total,
            self.dispatched.len(),
            self.skipped.len(),
            if flagged == 0 {
                String::new()
            } else {
                format!(", {flagged} batch{} flagged", if flagged == 1 { "" } else { "es" })
            }
        )
    }
}

/// A run as the history lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub at: DateTime<Utc>,
    pub trigger: String,
    pub total: usize,
    /// Per repository.
    #[serde(default)]
    pub totals: BTreeMap<String, usize>,
    pub dispatched: usize,
    pub skipped: usize,
    pub flagged: usize,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrendPoint {
    pub at: DateTime<Utc>,
    pub total: usize,
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LoopState {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_report: Option<Report>,
    #[serde(default)]
    pub history: Vec<RunSummary>,
    /// The flagged batches still on record.
    #[serde(default)]
    pub attention: Vec<AttentionItem>,
    #[serde(default)]
    pub targets: Vec<TargetRecord>,
    /// The last dispatch per repository (lowercased), for the cooldown.
    #[serde(default)]
    pub cooldowns: BTreeMap<String, DateTime<Utc>>,
    /// Totals per repository (lowercased), oldest first.
    #[serde(default)]
    pub trend: BTreeMap<String, Vec<TrendPoint>>,
}

pub struct Store {
    state: RwLock<LoopState>,
    file: PathBuf,
    persist: Mutex<()>,
    /// One run at a time, scheduled or pressed.
    running: Mutex<()>,
}

impl Store {
    pub fn new(config_dir: &FsPath) -> Self {
        let file = config_dir.join(FILE);
        let state = match std::fs::read(&file) {
            Err(_) => LoopState::default(),
            Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
                eprintln!("ts-any loop: could not parse {}: {e}; starting switched off", file.display());
                LoopState::default()
            }),
        };
        Store {
            state: RwLock::new(state),
            file,
            persist: Mutex::new(()),
            running: Mutex::new(()),
        }
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.state.read().await)?;
        write_atomic(&self.file, &data).await
    }

    pub async fn snapshot(&self) -> LoopState {
        self.state.read().await.clone()
    }
}

// --- the host: the mirror, node and the repository's TypeScript ---------------------------------

/// A repository's tree at one commit, in a scratch directory the count reads.
pub struct Checkout {
    pub sha: String,
    pub dir: PathBuf,
    /// Every path in the tree, repository-relative (the scratch copy holds only what the count
    /// and an install need).
    pub files: Vec<String>,
    /// Whether the directory is this checkout's to remove when it is dropped.
    pub owned: bool,
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Everything a run reads from outside itself. The real one reads the mirror and runs `node`;
/// the tests' fake answers from fixture directories, so no test touches the network.
pub trait Host: Send + Sync {
    fn org_repos(&self, org: &str) -> impl Future<Output = Result<Vec<String>>> + Send;
    /// The tree at `rev` (a branch), or at the default branch.
    fn checkout(&self, repo: &str, rev: Option<&str>) -> impl Future<Output = Result<Checkout>> + Send;
    /// The compiler-API count with the repository's own TypeScript; `Err` says why it could not
    /// run, and the token scan is used instead.
    fn typescript(
        &self,
        co: &Checkout,
        settings: &Settings,
    ) -> impl Future<Output = std::result::Result<TscOutput, String>> + Send;
}

/// Counts a checkout: the repository's TypeScript when it runs, the token scan otherwise.
pub async fn measure_checkout<H: Host>(host: &H, co: &Checkout, settings: &Settings) -> Measurement {
    match host.typescript(co, settings).await {
        Ok(tsc) => {
            let note = format!("the repository's TypeScript {} (compiler API)", tsc.version);
            let mut m = measure(&co.dir, &co.files, Some(&tsc), note);
            if settings.implicit && m.implicit.is_none() {
                m.notes.push("implicit any were not counted".to_string());
            }
            m
        }
        Err(why) => {
            let mut m = measure(&co.dir, &co.files, None, format!("token scan: {why}"));
            if settings.implicit {
                m.notes
                    .push("implicit any need the repository's TypeScript; not counted".to_string());
            }
            m
        }
    }
}

/// Where an executable is on this host: `PATH`, then `~/.cargo/bin`. Nothing is ever fetched.
fn which(binary: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".cargo/bin"));
    }
    dirs.into_iter().map(|d| d.join(binary)).find(|p| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// A repository-relative path that stays inside the scratch directory.
fn safe_rel(path: &str) -> Option<&str> {
    (!path.is_empty() && !path.starts_with('/') && !path.split('/').any(|p| p == ".." || p.is_empty())).then_some(path)
}

/// What the scratch copy holds: sources, JSON and YAML (tsconfigs, manifests, lockfiles), the
/// package managers' own files. Never `node_modules`.
fn copied(path: &str) -> bool {
    if path.split('/').any(|p| p == "node_modules" || p == ".git") {
        return false;
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    matches!(
        ext,
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "json" | "yaml" | "yml" | "lock" | "lockb"
    ) || matches!(file, ".npmrc" | ".yarnrc" | ".yarnrc.yml")
}

/// The repository's package manager, from the lockfile at its root.
pub fn package_manager(files: &[String]) -> std::result::Result<&'static str, String> {
    let has = |f: &str| files.iter().any(|p| p == f);
    if has("pnpm-lock.yaml") {
        Ok("pnpm")
    } else if has("yarn.lock") {
        Ok(if has(".yarnrc.yml") { "yarn-berry" } else { "yarn" })
    } else if has("package-lock.json") || has("npm-shrinkwrap.json") {
        Ok("npm")
    } else if has("bun.lock") || has("bun.lockb") {
        Err("bun has no offline-only install".to_string())
    } else {
        Err("no lockfile at the repository's root to install from".to_string())
    }
}

/// The compiler API (`typescript.js`) in the checkout's `node_modules`, at the root or beside a
/// tsconfig: `None` when no `typescript` package is installed, `Err` when the installed one has no
/// JavaScript API (TypeScript 7, the native compiler).
fn find_typescript(dir: &FsPath, files: &[String]) -> Option<std::result::Result<PathBuf, String>> {
    let mut dirs: Vec<PathBuf> = vec![dir.to_path_buf()];
    for cfg in files.iter().filter(|p| is_tsconfig(p)) {
        if let Some((parent, _)) = cfg.rsplit_once('/') {
            dirs.push(dir.join(parent));
        }
    }
    let mut native = None;
    for d in dirs {
        let pkg = d.join("node_modules/typescript");
        let api = pkg.join("lib/typescript.js");
        if api.is_file() {
            return Some(Ok(api));
        }
        if native.is_none()
            && let Ok(text) = std::fs::read_to_string(pkg.join("package.json"))
        {
            let version = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["version"].as_str().map(str::to_string))
                .unwrap_or_default();
            native = Some(Err(format!(
                "the repository's TypeScript {version} has no JavaScript compiler API (TypeScript 7 is the native compiler)"
            )));
        }
    }
    native
}

pub struct RealHost {
    pub app: Shared,
}

impl RealHost {
    /// Installs the repository's dependencies into the scratch copy with its own package manager,
    /// offline: only what the host's cache already holds, and no install scripts.
    async fn offline_install(&self, co: &Checkout) -> std::result::Result<(), String> {
        let pm = package_manager(&co.files)?;
        let bin = match pm {
            "yarn-berry" => "yarn",
            other => other,
        };
        let path = which(bin).ok_or_else(|| format!("{bin} is not installed on the host"))?;
        let mut cmd = tokio::process::Command::new(path);
        match pm {
            "npm" => cmd.args([
                "ci",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--loglevel=error",
            ]),
            "pnpm" => cmd.args([
                "install",
                "--offline",
                "--frozen-lockfile",
                "--ignore-scripts",
                "--reporter=silent",
            ]),
            "yarn" => cmd.args([
                "install",
                "--offline",
                "--frozen-lockfile",
                "--ignore-scripts",
                "--non-interactive",
                "--silent",
            ]),
            _ => cmd
                .args(["install", "--immutable", "--mode=skip-build"])
                .env("YARN_ENABLE_NETWORK", "0"),
        };
        // An offline install needs no credentials, and must not reach a registry.
        cmd.current_dir(&co.dir)
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .env_remove("NODE_AUTH_TOKEN")
            .env_remove("NPM_TOKEN")
            .env("npm_config_offline", "true")
            .env("CI", "1");
        crate::util::exec_within(INSTALL_LIMIT, &mut cmd)
            .await
            .map(|_| ())
            .map_err(|e| {
                let first = format!("{e:#}")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>();
                format!("the offline {bin} install failed (the host's cache may lack packages): {first}")
            })
    }
}

impl Host for RealHost {
    async fn org_repos(&self, org: &str) -> Result<Vec<String>> {
        crate::deps::org_repos(&self.app, org).await
    }

    async fn checkout(&self, repo: &str, rev: Option<&str>) -> Result<Checkout> {
        let app = &self.app;
        let bare = crate::code::ensure_bare(app, repo).await?;
        let (_, sha) = crate::code::resolve(app, &bare, rev).await?;
        let entries = crate::code::ls_tree(app, &bare, &sha).await?;
        let files: Vec<String> = entries.iter().map(|(_, _, p)| p.clone()).collect();
        let picked: Vec<(String, String)> = entries
            .into_iter()
            .filter(|(_, size, path)| *size <= MAX_FILE_BYTES && copied(path) && safe_rel(path).is_some())
            .map(|(blob, _, path)| (blob, path))
            .collect();
        let dir = app.cfg.data_dir.join("tmp").join(format!("ts-any-{}", short_id()));
        tokio::fs::create_dir_all(&dir).await?;
        // Made before anything can fail, so its drop removes the scratch copy either way.
        let co = Checkout {
            sha,
            dir,
            files,
            owned: true,
        };
        let blobs: Vec<String> = picked.iter().map(|(b, _)| b.clone()).collect();
        let mut contents: Vec<Option<Vec<u8>>> = vec![None; picked.len()];
        crate::code::cat_batch(app, &bare, &blobs, |i, bytes| contents[i] = Some(bytes.to_vec())).await?;
        for ((_, path), bytes) in picked.iter().zip(contents) {
            let Some(bytes) = bytes else { continue };
            let dest = co.dir.join(path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&dest, bytes).await?;
        }
        Ok(co)
    }

    async fn typescript(&self, co: &Checkout, settings: &Settings) -> std::result::Result<TscOutput, String> {
        let node = which("node").ok_or("node is not installed on the host")?;
        let ts_js = match find_typescript(&co.dir, &co.files) {
            Some(found) => found?,
            None if !settings.offline_install => {
                return Err("node_modules/typescript is absent and offline installs are off".to_string());
            }
            None => {
                self.offline_install(co).await?;
                find_typescript(&co.dir, &co.files).ok_or("typescript is not among the repository's dependencies")??
            }
        };
        let script = co.dir.join(".colonizer-ts-any.cjs");
        let list = co.dir.join(".colonizer-ts-any-files.json");
        let body = json!({
            "files": counted_files(&co.files),
            "tsconfigs": co.files.iter().filter(|p| is_tsconfig(p) && !not_counted(p)).collect::<Vec<_>>(),
        });
        tokio::fs::write(&script, TSC_SCRIPT).await.map_err(|e| e.to_string())?;
        tokio::fs::write(&list, body.to_string()).await.map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(node);
        cmd.arg("--max-old-space-size=4096")
            .arg(&script)
            .arg(&ts_js)
            .arg(&co.dir)
            .arg(&list)
            .arg(if settings.implicit { "1" } else { "0" })
            .current_dir(&co.dir)
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN");
        let (stdout, stderr) = crate::util::exec_capture(TOOL_LIMIT, &mut cmd)
            .await
            .map_err(|e| format!("{e:#}"))?;
        parse_tsc_output(&stdout).map_err(|e| {
            let said = stderr.lines().next().unwrap_or_default();
            format!(
                "the TypeScript count failed: {e:#}{}",
                if said.is_empty() { String::new() } else { format!(": {said}") }
            )
        })
    }
}

// --- a run --------------------------------------------------------------------------------------

/// The repositories the allowlist covers: each named repository, and each org's repositories.
async fn covered<H: Host>(host: &H, settings: &Settings, notes: &mut Vec<String>) -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    for entry in &settings.allow {
        let found = if entry.contains('/') {
            vec![entry.clone()]
        } else {
            match host.org_repos(entry).await {
                Ok(list) => list,
                Err(e) => {
                    notes.push(format!("could not list the repositories of {entry}: {e:#}"));
                    Vec::new()
                }
            }
        };
        for r in found {
            if !repos.iter().any(|x| x.eq_ignore_ascii_case(&r)) {
                repos.push(r);
            }
        }
    }
    repos
}

/// Counts one repository at its default branch. Never fails: what went wrong is the report's.
pub async fn check_repo<H: Host>(host: &H, repo: &str, settings: &Settings) -> (RepoReport, Option<Measurement>) {
    let mut report = RepoReport {
        repo: repo.to_string(),
        ..RepoReport::default()
    };
    let co = match host.checkout(repo, None).await {
        Ok(co) => co,
        Err(e) => {
            report.error = Some(format!("could not read the mirror: {e:#}"));
            return (report, None);
        }
    };
    report.sha = Some(co.sha.clone()).filter(|s| !s.is_empty());
    if !co.files.iter().any(|p| is_tsconfig(p) && !not_counted(p)) {
        report
            .notes
            .push("no tsconfig.json: not a TypeScript repository, nothing counted".to_string());
        return (report, None);
    }
    report.typescript = true;
    let m = measure_checkout(host, &co, settings).await;
    report.method = Some(m.method);
    report.method_note = Some(m.method_note.clone());
    report.ts_version = m.ts_version.clone();
    report.total = m.total;
    report.implicit = m.implicit;
    report.as_casts = m.as_casts;
    report.suppressions = m.suppressions;
    report.ts_files = m.ts_files;
    report.forms = m.forms.clone();
    report.modules = m.modules.iter().take(TOP_MODULES).cloned().collect();
    report.files = m
        .files
        .iter()
        .filter(|f| f.explicit > 0)
        .take(MAX_STORED_FILES)
        .cloned()
        .collect();
    report.notes.extend(m.notes.iter().cloned());
    (report, Some(m))
}

/// Starts one batch colony through the normal admission path.
async fn dispatch(app: &Shared, t: &Target, autopilot: bool) -> Result<Session, crate::AppError> {
    let body = json!({
        "repo": t.repo,
        "title": t.title(),
        "instructions": brief(t),
        "autopilot": autopilot,
        "origin": t.origin(),
    });
    let req: NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let Json(session) = sessions::create(State(app.clone()), None, Json(req)).await?;
    Ok(session)
}

/// Recounts the batches whose pull requests were published since the last run: at most
/// [`MAX_CHECKS_PER_RUN`], one at a time. Returns the records' new checks by session.
async fn post_checks<H: Host>(
    host: &H,
    records: &[TargetRecord],
    sessions: &[Session],
    settings: &Settings,
    now: DateTime<Utc>,
) -> Vec<(String, PostCheck)> {
    let mut out = Vec::new();
    for rec in records.iter().filter(|r| r.check.is_none()) {
        if out.len() >= MAX_CHECKS_PER_RUN {
            break;
        }
        let session = sessions.iter().find(|s| s.id == rec.session);
        let no_pr = |note: &str| PostCheck {
            at: now,
            head: None,
            module_before: rec.before.module_explicit,
            module_after: None,
            suppressions_added: 0,
            as_casts_added: 0,
            any_added_elsewhere: 0,
            flagged: false,
            problems: Vec::new(),
            note: Some(note.to_string()),
        };
        let Some(s) = session else {
            out.push((rec.session.clone(), no_pr("the colony is gone; nothing to check")));
            continue;
        };
        if s.pr_url.is_none() {
            if !still_open(s) {
                out.push((rec.session.clone(), no_pr("no pull request was published")));
            }
            continue;
        }
        if s.status.busy() {
            continue;
        }
        let (co, note) = match host.checkout(&rec.repo, Some(&s.branch)).await {
            Ok(co) => (co, None),
            Err(_) if s.status == SessionStatus::Merged => match host.checkout(&rec.repo, None).await {
                Ok(co) => (
                    co,
                    Some("the branch is gone after the merge; the default branch was counted".to_string()),
                ),
                Err(e) => {
                    eprintln!("ts-any loop: could not read {} for the post-check: {e:#}", rec.repo);
                    continue;
                }
            },
            Err(e) => {
                eprintln!(
                    "ts-any loop: could not read {}@{} for the post-check: {e:#}",
                    rec.repo, s.branch
                );
                continue;
            }
        };
        let m = measure_checkout(host, &co, settings).await;
        let mut check = post_check(rec, &m, &co.sha, now);
        check.note = note;
        out.push((rec.session.clone(), check));
    }
    out
}

/// What a run is asked to do.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RunRequest {
    #[serde(default)]
    pub dry_run: bool,
    /// Count this one repository. A dry run may name any repository; a real run only one the
    /// allowlist covers.
    #[serde(default)]
    pub repo: Option<String>,
}

/// One run: count every covered repository, recount the published batches, pick one batch per
/// repository and — unless it is a dry run — start them, store the report and write the activity log.
pub async fn run_once<H: Host>(app: &Shared, host: &H, req: &RunRequest, trigger: &str, now: DateTime<Utc>) -> Report {
    let state = app.ts_any.snapshot().await;
    let mut settings = state.settings.clone();
    // `*` is every visible org, resolved at use time so a hidden org is left out (issue #1213).
    settings.allow = app.resolve_scope(&settings.allow).await;
    let started_at = Utc::now();
    let mut notes = Vec::new();
    let repos = match &req.repo {
        Some(repo) => vec![repo.clone()],
        None => covered(host, &settings, &mut notes).await,
    };
    let sessions = app.sessions.read().await.clone();
    let mut reports = Vec::new();
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    // One repository at a time: a checkout and a count each, never a burst.
    for repo in &repos {
        let (mut report, m) = check_repo(host, repo, &settings).await;
        report.previous = state
            .trend
            .get(&repo.to_ascii_lowercase())
            .map(|points| points.iter().rev().take(5).map(|p| p.total).collect())
            .unwrap_or_default();
        if let Some(m) = &m {
            if m.total == 0 {
                skipped.push(Skipped {
                    repo: repo.clone(),
                    module: None,
                    reason: "no explicit any to remove".to_string(),
                });
            } else {
                let (target, held) = select(
                    repo,
                    report.sha.as_deref().unwrap_or_default(),
                    m,
                    settings.batch_cap as usize,
                    &sessions,
                );
                skipped.extend(held);
                match target {
                    Some(t) => targets.push(t),
                    None => skipped.push(Skipped {
                        repo: repo.clone(),
                        module: None,
                        reason: "every module with an explicit any already has a colony on it".to_string(),
                    }),
                }
            }
        }
        reports.push(report);
    }
    let checks = if req.dry_run {
        Vec::new()
    } else {
        post_checks(host, &state.targets, &sessions, &settings, now).await
    };
    let blocked = authority::external_writes_blocked();
    let (go, held) = plan_dispatch(targets, &settings, &state.cooldowns, now, blocked);
    skipped.extend(held);
    let mut dispatched = Vec::new();
    let mut records = Vec::new();
    for t in &go {
        let mut d = Dispatched {
            repo: t.repo.clone(),
            module: t.module.clone(),
            session: None,
            title: t.title(),
            occurrences: t.occurrences.len(),
            module_total: t.module_total,
        };
        if req.dry_run {
            dispatched.push(d);
            continue;
        }
        match dispatch(app, t, settings.autopilot).await {
            Ok(session) => {
                records.push(TargetRecord {
                    session: session.id.clone(),
                    repo: t.repo.clone(),
                    module: t.module.clone(),
                    sha: t.sha.clone(),
                    at: now,
                    occurrences: t.occurrences.iter().map(|o| format!("{}:{}", o.file, o.line)).collect(),
                    before: t.before.clone(),
                    check: None,
                });
                d.session = Some(session.id.clone());
                dispatched.push(d);
                let mut entry = Entry::new("loop.ts_any", "colony").colony(&session);
                entry.target = Some(NAME.to_string());
                entry.section = Some("loops".to_string());
                entry.detail = Some(format!(
                    "{} of {} explicit any in {}",
                    t.occurrences.len(),
                    t.module_total,
                    t.module
                ));
                crate::activity::record(app, entry).await;
            }
            Err(e) => skipped.push(Skipped {
                repo: t.repo.clone(),
                module: Some(t.module.clone()),
                reason: format!("could not start the colony: {}", e.message()),
            }),
        }
    }
    // The checks made this run, as lines and as attention.
    let pr_of = |id: &str| sessions.iter().find(|s| s.id == id).and_then(|s| s.pr_url.clone());
    let mut check_lines = Vec::new();
    let mut attention = Vec::new();
    for (session, check) in &checks {
        let Some(rec) = state.targets.iter().find(|r| &r.session == session) else {
            continue;
        };
        check_lines.push(CheckLine {
            session: session.clone(),
            repo: rec.repo.clone(),
            module: rec.module.clone(),
            pr_url: pr_of(session),
            flagged: check.flagged,
            summary: check.summary(),
        });
        let checked = TargetRecord {
            check: Some(check.clone()),
            ..rec.clone()
        };
        attention.extend(attention_of(&checked, pr_of(session)));
    }
    if repos.is_empty() && notes.is_empty() {
        notes.push("nothing is opted in: add an org or a repository to the allowlist".to_string());
    }
    if req.dry_run {
        notes.push("a dry run recounts no published batch".to_string());
    }
    let mut report = Report {
        id: format!("tsa_{}", short_id()),
        started_at,
        finished_at: Utc::now(),
        dry_run: req.dry_run,
        trigger: trigger.to_string(),
        blocked,
        total: reports.iter().map(|r| r.total).sum(),
        repos: reports,
        dispatched,
        skipped,
        checks: check_lines,
        attention,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    };
    report.finished_at = Utc::now();
    if req.dry_run {
        return report;
    }
    let summary = report.summary();
    {
        let mut st = app.ts_any.state.write().await;
        for rec in &records {
            st.cooldowns.insert(rec.repo.to_ascii_lowercase(), rec.at);
        }
        for (session, check) in &checks {
            if let Some(rec) = st.targets.iter_mut().find(|r| &r.session == session) {
                rec.check = Some(check.clone());
            }
        }
        st.targets.extend(records);
        // A record is kept while its colony is still open, and for a month after it was made.
        st.targets.retain(|r| {
            now - r.at < ChronoDuration::days(RECORD_DAYS) || sessions.iter().any(|s| s.id == r.session && still_open(s))
        });
        st.attention = st.targets.iter().filter_map(|r| attention_of(r, pr_of(&r.session))).collect();
        for r in report.repos.iter().filter(|r| r.typescript && r.error.is_none()) {
            let points = st.trend.entry(r.repo.to_ascii_lowercase()).or_default();
            points.push(TrendPoint {
                at: report.started_at,
                total: r.total,
                sha: r.sha.clone(),
            });
            let excess = points.len().saturating_sub(TREND);
            points.drain(..excess);
        }
        st.history.insert(
            0,
            RunSummary {
                id: report.id.clone(),
                at: report.started_at,
                trigger: trigger.to_string(),
                total: report.total,
                totals: report
                    .repos
                    .iter()
                    .filter(|r| r.typescript)
                    .map(|r| (r.repo.clone(), r.total))
                    .collect(),
                dispatched: report.dispatched.len(),
                skipped: report.skipped.len(),
                flagged: report.attention.len(),
                summary: summary.clone(),
            },
        );
        st.history.truncate(HISTORY);
        st.last_report = Some(report.clone());
    }
    if let Err(e) = app.ts_any.save().await {
        eprintln!("ts-any loop: could not save {}: {e:#}", app.ts_any.file.display());
    }
    let mut entry = Entry::new("loop.ts_any", "colony");
    entry.target = Some(NAME.to_string());
    entry.section = Some("loops".to_string());
    entry.detail = Some(summary);
    if report.repos.len() == 1 {
        entry.repo = Some(report.repos[0].repo.clone());
        entry.org = report.repos[0].repo.split('/').next().map(str::to_string);
    }
    crate::activity::record(app, entry).await;
    crate::loop_history::record(app, crate::loop_history::from_ts_any(&report)).await;
    for item in &report.attention {
        eprintln!("ts-any loop: attention: {}", item.reason);
    }
    report
}

/// Fires the loop when it is due. A run already in progress (a pressed one) skips this tick.
pub(crate) async fn tick<H: Host>(app: &Shared, host: &H, now: DateTime<Utc>) -> Option<Report> {
    let (active, due, cadence) = {
        let st = app.ts_any.state.read().await;
        (
            st.settings.active(),
            st.next_run_at.is_some_and(|at| at <= now),
            st.settings.cadence.clone(),
        )
    };
    if !active || !due {
        return None;
    }
    let _running = app.ts_any.running.try_lock().ok()?;
    // Book the next slot first, so a run that takes long cannot fire twice.
    app.ts_any.state.write().await.next_run_at = Some(next_run_after(&cadence, now));
    Some(run_once(app, host, &RunRequest::default(), "schedule", now).await)
}

async fn run(app: Shared) {
    let mut every = tokio::time::interval(Duration::from_secs(60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        every.tick().await;
        let host = RealHost { app: app.clone() };
        tick(&app, &host, Utc::now()).await;
    }
}

// --- routes -------------------------------------------------------------------------------------

async fn view(app: &App) -> Value {
    let st = app.ts_any.snapshot().await;
    json!({
        "name": NAME,
        "settings": st.settings,
        "next_run_at": st.next_run_at.filter(|_| st.settings.active()),
        "running": app.ts_any.running.try_lock().is_err(),
        "node": which("node").is_some(),
        "blocked": authority::external_writes_blocked(),
        "last_report": st.last_report,
        "history": st.history,
        "attention": st.attention,
        "trend": st.trend,
    })
}

/// `GET /api/ts-any-loop`: the loop's settings, the last report, the history and the trend.
pub async fn get(State(app): State<Shared>) -> Json<Value> {
    Json(view(&app).await)
}

/// `PUT /api/ts-any-loop`: replaces the settings; the next run is booked from now.
pub async fn put(State(app): State<Shared>, Json(settings): Json<Settings>) -> ApiResult<Value> {
    let settings = settings.validated().map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    {
        let mut st = app.ts_any.state.write().await;
        st.next_run_at = settings.active().then(|| next_run_after(&settings.cadence, Utc::now()));
        st.settings = settings;
    }
    app.ts_any.save().await?;
    Ok(Json(view(&app).await))
}

/// `POST /api/ts-any-loop/run`: a run now, or a dry run that lists the counts and what it would
/// dispatch and writes nothing. One run at a time.
pub async fn run_now(State(app): State<Shared>, body: Option<Json<RunRequest>>) -> ApiResult<Report> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    if let Some(repo) = &req.repo {
        if !valid_repo(repo) {
            return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
        }
        if !req.dry_run
            && !{
                let mut settings = app.ts_any.state.read().await.settings.clone();
                settings.allow = app.resolve_scope(&settings.allow).await;
                settings.covers(repo)
            }
        {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                &format!("{repo} is not on the TypeScript any loop's allowlist; add it, or ask for a dry run"),
            ));
        }
    }
    let Ok(_running) = app.ts_any.running.try_lock() else {
        return Err(client_error(
            StatusCode::CONFLICT,
            "a TypeScript any run is already in progress",
        ));
    };
    let host = RealHost { app: app.clone() };
    Ok(Json(run_once(&app, &host, &req, "manual", Utc::now()).await))
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move { run(app).await });
}

pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/ts-any-loop", routing::get(get).put(put))
        .route("/api/ts-any-loop/run", routing::post(run_now))
}

#[cfg(test)]
mod tests;
