//! Loop prompt templates: the canned prompts `colonizer loop create --template` builds a loop's
//! prompt from — the same text the cockpit's template picker offers (`web/src/cockpit/loops.ts`).
//! Each template is a struct of the repository's inputs, so the operator's flags override the
//! defaults field by field and the prompt is rendered from one place.

/// The `data-refresh` template's inputs. The defaults are what the template's own instructions tell
/// an operator to add to their repository; `loop create --template data-refresh --sources …`
/// overrides them per loop.
pub struct DataRefresh {
    /// The JSON list of sources the loop refreshes.
    pub sources: String,
    /// The command that turns fetched evidence into the source's changed values, `<id>` for the
    /// source id.
    pub extract: String,
    /// The command that must accept the applied changes.
    pub validate: String,
    /// The command that reads the change set on stdin and prints `auto` or `review`.
    pub policy: String,
    /// Where fetched evidence is kept.
    pub evidence: String,
    /// Failed fetches before a source is reported broken.
    pub max_failures: u32,
}

impl Default for DataRefresh {
    fn default() -> Self {
        Self {
            sources: "data/sources.json".into(),
            extract: "npm run extract -- <id>".into(),
            validate: "npm run validate".into(),
            policy: "npm run --silent refresh-policy".into(),
            evidence: "evidence/".into(),
            max_failures: 3,
        }
    }
}

impl DataRefresh {
    /// The loop's prompt with the inputs filled in: byte for byte the cockpit template's text, so
    /// a loop made from the terminal and one made in the cockpit brief their colonies alike.
    pub fn prompt(&self) -> String {
        // Step 3's path sits *inside* the evidence directory, so the trailing slash the Inputs line
        // shows (the default is `evidence/`) must not double up there.
        let dir = self.evidence.trim_end_matches('/');
        let dir = if dir.is_empty() { self.evidence.as_str() } else { dir };
        format!(
            "Refresh this repository's data files from their external sources, and open one pull request with the evidence.\n\
\n\
Inputs (paths and commands of this repository):\n\
- Sources file: `{sources}`, a JSON list of `{{\"id\", \"url\", \"method\", \"cadence\" or \"volatility\"}}`. This loop also keeps `verified_on` (a date) and `failures` (a count) on each entry.\n\
- Extract command: `{extract}`, with `<id>` replaced by the source id. It reads the fetched evidence and prints the source's changed values as JSON.\n\
- Validate command: `{validate}`.\n\
- Policy command: `{policy}`. Given the change set as JSON on stdin, it prints `auto` or `review`.\n\
- Evidence directory: `{evidence}`.\n\
\n\
Each run:\n\
1. Load the sources file and choose the due shard: the entries whose cadence or volatility class makes them due since their `verified_on`. When this run's parameters name `only` sources, the shard is exactly those ids, due or not.\n\
2. Fetch each source by its `method`: `http` with curl, `browser` with a headless browser for JS-rendered pages (install Playwright's Chromium in the VM if it is missing).\n\
3. Save what you fetched to `{dir}/<id>/<UTC timestamp>.<ext>` with its sha256 next to it as `.sha256`, then run the extract command.\n\
4. When a value changed, fetch and extract that source again at least 2 minutes later, and keep the change only when the two results agree. Leave an unconfirmed change out of the pull request and list it (source id, both values) under \"Not confirmed\" in the description.\n\
5. Apply the confirmed changes and run the validate command; drop any change it rejects and say why. Write the pull request description: a change table (source, field, old → new, % change for numbers) and, for each changed source, its URL, fetch time, evidence path with its sha256, and the extractor version (what the extractor prints, else `git log -1 --format=%h` of its files).\n\
6. Run the policy command and write `data-refresh:auto` or `data-refresh:review`, from its output, to `/harness/out/pr-labels`; the harness labels the pull request with it.\n\
\n\
Unchanged sources: set their `verified_on` to today. These bumps go into the same pull request as the changes or, when nothing changed, into one \"verified on <date>\" pull request; never more than one pull request per run.\n\
\n\
Failures: when a source cannot be fetched or extracted, add 1 to its `failures` in the sources file (set it back to 0 after a success) and list the error in the description. When `failures` reaches {max_failures}, report it with the finding tool, titled exactly `source-broken:<id>`, with the error and the last evidence path; if `/colonizer/github/issues.json` lists an open issue with that title, comment on it instead.",
            sources = self.sources,
            extract = self.extract,
            validate = self.validate,
            policy = self.policy,
            evidence = self.evidence,
            max_failures = self.max_failures,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendered default is the cockpit template's text byte for byte (`web/src/cockpit/loops.ts`,
    /// the "Refresh data files from their sources" template), so a loop made from the terminal and
    /// one made in the cockpit brief their colonies alike.
    #[test]
    fn the_default_prompt_is_the_cockpit_template_s_text() {
        let prompt = DataRefresh::default().prompt();
        let expected = r#"Refresh this repository's data files from their external sources, and open one pull request with the evidence.

Inputs (paths and commands of this repository):
- Sources file: `data/sources.json`, a JSON list of `{"id", "url", "method", "cadence" or "volatility"}`. This loop also keeps `verified_on` (a date) and `failures` (a count) on each entry.
- Extract command: `npm run extract -- <id>`, with `<id>` replaced by the source id. It reads the fetched evidence and prints the source's changed values as JSON.
- Validate command: `npm run validate`.
- Policy command: `npm run --silent refresh-policy`. Given the change set as JSON on stdin, it prints `auto` or `review`.
- Evidence directory: `evidence/`.

Each run:
1. Load the sources file and choose the due shard: the entries whose cadence or volatility class makes them due since their `verified_on`. When this run's parameters name `only` sources, the shard is exactly those ids, due or not.
2. Fetch each source by its `method`: `http` with curl, `browser` with a headless browser for JS-rendered pages (install Playwright's Chromium in the VM if it is missing).
3. Save what you fetched to `evidence/<id>/<UTC timestamp>.<ext>` with its sha256 next to it as `.sha256`, then run the extract command.
4. When a value changed, fetch and extract that source again at least 2 minutes later, and keep the change only when the two results agree. Leave an unconfirmed change out of the pull request and list it (source id, both values) under "Not confirmed" in the description.
5. Apply the confirmed changes and run the validate command; drop any change it rejects and say why. Write the pull request description: a change table (source, field, old → new, % change for numbers) and, for each changed source, its URL, fetch time, evidence path with its sha256, and the extractor version (what the extractor prints, else `git log -1 --format=%h` of its files).
6. Run the policy command and write `data-refresh:auto` or `data-refresh:review`, from its output, to `/harness/out/pr-labels`; the harness labels the pull request with it.

Unchanged sources: set their `verified_on` to today. These bumps go into the same pull request as the changes or, when nothing changed, into one "verified on <date>" pull request; never more than one pull request per run.

Failures: when a source cannot be fetched or extracted, add 1 to its `failures` in the sources file (set it back to 0 after a success) and list the error in the description. When `failures` reaches 3, report it with the finding tool, titled exactly `source-broken:<id>`, with the error and the last evidence path; if `/colonizer/github/issues.json` lists an open issue with that title, comment on it instead."#;
        assert_eq!(prompt, expected);
    }

    /// The flags override the defaults, and the overrides reach every place the input is named.
    #[test]
    fn the_flags_override_the_defaults() {
        let template = DataRefresh {
            sources: "src/list.json".into(),
            evidence: "out/ev".into(),
            max_failures: 7,
            ..Default::default()
        };
        let prompt = template.prompt();
        assert!(prompt.contains("- Sources file: `src/list.json`"), "{prompt}");
        assert!(prompt.contains("to `out/ev/<id>/<UTC timestamp>.<ext>`"), "{prompt}");
        assert!(prompt.contains("When `failures` reaches 7"), "{prompt}");
        assert!(!prompt.contains("data/sources.json"), "{prompt}");
    }
}
