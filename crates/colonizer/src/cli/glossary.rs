//! `colonizer help`: the `help` command, and the glossary it can print (issue #904).
//!
//! clap generates a `help` subcommand of its own, and [`crate::cli::Cli`] turns that off
//! (`disable_help_subcommand`) so a `glossary` topic can live under it. Everything the generated
//! one did still happens here: `colonizer help` prints the top-level help and `colonizer help
//! <command>` prints that command's own, through [`rendered_help`], which is the same parse
//! `<command> --help` is — so the two cannot drift.
//!
//! The glossary itself is the other half of this module. The CLI talks about colonies, settlers
//! and a mothership, which are metaphors rather than things anyone has seen before. Each entry in
//! [`ENTRIES`] gives the ordinary term first and the name this CLI uses for it in parentheses, so
//! a newcomer can read `colonizer --help` and come away with the right picture: a colony is a
//! sandbox, a settler is a helper an agent calls, and the mothership is the service everything else
//! talks to. [`render`] lays the entries out, so the snapshot in `help.snap` and the words the help
//! text uses cannot drift apart without the test saying so.

use crate::cli::{EXIT_OK, EXIT_USAGE};
use clap::{CommandFactory as _, Subcommand};

/// `colonizer help <topic>`.
#[derive(Subcommand, Debug)]
pub(crate) enum HelpCommand {
    /// Plain-language glossary: what a colony, a settler and the mothership actually are
    Glossary,
    /// Any other command's help, spelled as `colonizer help <command>` — the form clap's own
    /// `help` took, kept working by [`rendered_help`].
    #[command(external_subcommand)]
    Command(Vec<String>),
}

/// `colonizer help`, `colonizer help <command>`, `colonizer help glossary`.
///
/// The first two are what clap's own generated `help` subcommand did, and both still print the same
/// text and exit 0. A name no command answers is a usage error, as it was before.
pub(crate) fn help_command(command: Option<HelpCommand>) -> i32 {
    match command {
        None => {
            // The empty path is the top level, which always resolves.
            let help = rendered_help(&[]).expect("the top-level help always renders");
            print!("{help}");
            EXIT_OK
        }
        Some(HelpCommand::Glossary) => {
            print!("{}", render());
            EXIT_OK
        }
        Some(HelpCommand::Command(names)) => match rendered_help(&names) {
            Ok(help) => {
                print!("{help}");
                EXIT_OK
            }
            Err(err) => {
                let _ = err.print();
                EXIT_USAGE
            }
        },
    }
}

/// clap's own rendered help for the command at `path`, the empty path being the top level.
///
/// `colonizer help <command>` is therefore exactly the text `colonizer <command> --help` prints,
/// nested commands included, because it is that parse. A path no command answers is the error clap
/// printed for `colonizer help <path>`.
pub(crate) fn rendered_help(path: &[String]) -> std::result::Result<String, clap::Error> {
    use crate::cli::{Cli, try_parse_from};

    if path.is_empty() {
        return Ok(Cli::command().render_help().to_string());
    }
    // Asking clap to parse `<command…> --help` is how the generated `help` subcommand rendered it,
    // and it is the only way to get the whole thing: the usage line, the global flags propagated
    // down and the hidden refusals `cli_command` installs. Walking the tree by hand misses all three.
    let mut args = vec!["colonizer".to_string()];
    args.extend(path.iter().cloned());
    args.push("--help".to_string());
    match try_parse_from(args) {
        Ok(_) => unreachable!("--help displays rather than parsing"),
        // An unknown command reaches clap as an unknown subcommand, and its error is what the
        // generated `help` subcommand printed too — passed on rather than rewritten.
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => Ok(error.to_string()),
        Err(error) => Err(error),
    }
}

/// One glossary entry: the plain term, the name this CLI gives it, and what it actually is.
pub(crate) struct Entry {
    /// The ordinary words for it. What a newcomer would call it.
    pub(crate) plain: &'static str,
    /// The name this CLI and its docs use, shown in parentheses after [`Self::plain`].
    pub(crate) jargon: &'static str,
    /// What it is, wrapped to the width [`render`] indents. Every line starts unindented.
    pub(crate) body: &'static str,
}

/// The one line under the title, before the entries.
const INTRO: &str = "This CLI describes itself in metaphors. Here is each one next to the \
                     ordinary words for it.";

/// The entries, in the order a newcomer meets them: what is run, what runs inside it, and what
/// runs it all.
pub(crate) const ENTRIES: &[Entry] = &[
    Entry {
        plain: "isolated sandbox",
        jargon: "colony",
        body: "\
A throwaway microVM — a tiny virtual machine with its own kernel — in which one
coding agent works on a repository. Nothing it does reaches the machine you sit
at: it has its own filesystem, its own credentials and its own network rules, and
stopping it throws the whole thing away. `launch` starts one; `list`, `status`,
`logs` and `diff` read what it did; `stop` ends it and `resume` picks its worktree
back up where it was left.",
    },
    Entry {
        plain: "worker agent",
        jargon: "settler",
        body: "\
A helper the agent inside a colony calls for one slice of its work: read these
files, run the tests, write this one fix. Settlers are short-lived — a colony
spawns them, they report back to the colony that spawned them, and they die with
it. A settler never opens a pull request and never outlives its sandbox; the
colony is the one that talks to you and does the publishing.",
    },
    Entry {
        plain: "control plane",
        jargon: "mothership",
        body: "\
The `colonizer` process that starts sandboxes, watches them and routes your
commands to the right one. One runs per machine, and `colonizer` with no
subcommand at all starts it. Everything else — the cockpit in a browser, every
client command, the API other tools call — talks to it over `--host`, so the
same commands work against a control plane on another machine across a tailnet.",
    },
    Entry {
        plain: "record of one sandbox",
        jargon: "session",
        body: "\
What the control plane keeps about a colony: its id, state, worktree, logs and
transcript. That id is what `status`, `logs`, `diff`, `ask`, `answer`, `stop` and
`resume` all take, so `list` is where you find it.",
    },
    Entry {
        plain: "scheduled prompt",
        jargon: "loop",
        body: "\
A saved prompt plus a cadence that launches a colony on it —
`colonizer loop create acme/app --name Triage --prompt '...' daily@09:00`.
Loops are for work that should happen while you are not watching: triage, sweeps,
anything that would otherwise be a cron job and a forgotten terminal.",
    },
];

/// The glossary, as `colonizer help glossary` prints it.
pub(crate) fn render() -> String {
    let mut out = String::from("Plain-language glossary\n\n");
    out.push_str(INTRO);
    out.push('\n');
    for entry in ENTRIES {
        // The heading is plain first, jargon in parentheses: read down the left edge and you get
        // the ordinary words, with only this CLI's name to look up.
        out.push_str(&format!("\n  {} ({})\n", entry.plain, entry.jargon));
        for line in entry.body.lines() {
            if !line.is_empty() {
                out.push_str("      ");
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command, try_parse_from};
    use clap::error::ErrorKind;
    use std::path::Path;

    /// `colonizer help`'s own parse, as the binary makes it.
    fn parse(args: &[&str]) -> Cli {
        try_parse_from(std::iter::once("colonizer").chain(args.iter().copied())).expect("the arguments should parse")
    }

    /// The rendered `--help` of a subcommand, through the same parse the binary uses.
    fn help_for(command: &str) -> String {
        let err = try_parse_from(["colonizer", command, "--help"]).unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::DisplayHelp),
            "{command} --help should display"
        );
        err.to_string()
    }

    /// Turning off clap's own `help` subcommand must not change what `colonizer help` printed
    /// (issue #904): the top-level help, and each command's help under `help <command>` — which is
    /// byte for byte what `<command> --help` prints.
    #[test]
    fn help_still_prints_what_clap_generated() {
        assert!(matches!(parse(&["help"]).command, Some(Command::Help { command: None })));

        // `colonizer help <command>` renders the same text as `colonizer <command> --help`.
        for command in ["version", "status", "launch", "login-item", "man", "completions"] {
            let Some(Command::Help {
                command: Some(HelpCommand::Command(names)),
            }) = parse(&["help", command]).command
            else {
                panic!("`help {command}` should carry the command's name");
            };
            assert_eq!(names, &[command.to_string()]);
            assert_eq!(
                rendered_help(&names).unwrap(),
                help_for(command),
                "`help {command}` should print what `{command} --help` prints"
            );
        }

        // Nested commands still resolve, as they did under clap's generated subcommand.
        let help = rendered_help(&["loop".into(), "create".into()]).unwrap();
        assert!(help.contains("colonizer loop create"), "{help}");
    }

    /// `colonizer help glossary` reaches the glossary, and the topic clap would have rejected is
    /// the one we added.
    #[test]
    fn help_glossary_reaches_the_glossary() {
        assert!(matches!(
            parse(&["help", "glossary"]).command,
            Some(Command::Help {
                command: Some(HelpCommand::Glossary)
            })
        ));
        let glossary = render();
        for term in ["isolated sandbox", "worker agent", "control plane"] {
            assert!(glossary.contains(term), "the glossary should carry {term:?}:\n{glossary}");
        }
    }

    /// A name no command answers is a usage error, as it was under clap's generated subcommand.
    #[test]
    fn help_of_an_unknown_command_is_a_usage_error() {
        let err = rendered_help(&["nosuchthing".into()]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand, "{err}");
        assert_eq!(err.exit_code(), EXIT_USAGE);
    }

    /// The three plain terms the acceptance names, each paired with the word it explains.
    #[test]
    fn the_plain_terms_come_first_and_name_their_jargon() {
        let glossary = render();
        for (plain, jargon) in [
            ("isolated sandbox", "colony"),
            ("worker agent", "settler"),
            ("control plane", "mothership"),
        ] {
            let heading = format!("{plain} ({jargon})");
            assert!(
                glossary.contains(&heading),
                "the glossary should carry {heading:?}:\n{glossary}"
            );
            // Plain term first: the heading starts with it, and the jargon only follows inside
            // the parentheses.
            assert!(
                glossary.contains(&format!("\n  {heading}\n")),
                "{heading:?} should be a heading of its own:\n{glossary}"
            );
        }
    }

    /// Every entry names its jargon, so `help glossary` is a key from this CLI's words to ordinary
    /// ones and not a list of prose.
    #[test]
    fn every_entry_explains_its_jargon() {
        for entry in ENTRIES {
            let heading = format!("{} ({})", entry.plain, entry.jargon);
            assert!(
                render().contains(&format!("\n  {heading}\n")),
                "{heading:?} is missing its heading"
            );
            assert!(entry.body.len() > 120, "{} should say more than a definition", entry.jargon);
        }
    }

    /// The render is stable and newline-terminated: the snapshot compares it byte for byte.
    #[test]
    fn the_render_is_newline_terminated_and_has_no_trailing_blank() {
        let glossary = render();
        assert!(glossary.ends_with("a forgotten terminal.\n"), "{glossary}");
        assert!(!glossary.contains("\n\n\n"), "{glossary}");
        assert!(glossary.starts_with("Plain-language glossary\n"), "{glossary}");
    }

    /// The plain terms reach the top-level help, not only the glossary: a newcomer reads `--help`
    /// first and may never run `help glossary`, so its own words should be glossed there.
    #[test]
    fn the_plain_terms_reach_the_help_that_names_the_jargon() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("isolated sandbox"), "{help}");
        assert!(help.contains("colonizer help glossary"), "{help}");
        for command in ["launch", "status", "token", "login-item"] {
            let mut cli = Cli::command();
            let about = cli
                .find_subcommand_mut(command)
                .unwrap_or_else(|| panic!("{command} is a command"))
                .render_help()
                .to_string();
            assert!(
                about.contains("isolated sandbox") || about.contains("control plane"),
                "`{command} --help` should lead with a plain term:\n{about}"
            );
        }
    }

    /// The snapshot file's path, next to this module.
    fn snapshot_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/help.snap")
    }

    /// What the two help outputs look like today, as one newline-terminated string.
    ///
    /// `colonizer --help` rather than `colonizer help`: the two print the same text, and going
    /// through clap's own parse keeps the snapshot honest about what the operator sees.
    fn snapshot_body() -> String {
        let help = try_parse_from(["colonizer", "--help"]).unwrap_err().to_string();
        format!("### colonizer --help\n{help}\n### colonizer help glossary\n{}", render())
            .trim_end()
            .to_string()
            + "\n"
    }

    /// The help output is snapshotted, so rewording it — or losing a plain term — is a visible
    /// diff rather than a silent change to what a newcomer reads.
    ///
    /// Regenerate with `UPDATE_HELP_SNAPSHOT=1 cargo test -p colonizer-harness help_output`.
    #[test]
    fn the_help_output_matches_its_snapshot() {
        let want = snapshot_body();
        let path = snapshot_path();
        if std::env::var_os("UPDATE_HELP_SNAPSHOT").is_some() {
            std::fs::write(&path, &want).unwrap();
            return;
        }
        let got = std::fs::read_to_string(&path).unwrap_or_default();
        if got != want {
            let old: std::collections::BTreeSet<&str> = got.lines().collect();
            let new: std::collections::BTreeSet<&str> = want.lines().collect();
            let gone: Vec<_> = old.difference(&new).collect();
            let added: Vec<_> = new.difference(&old).collect();
            panic!(
                "the help output changed; if that is intended, regenerate the snapshot with \
                 UPDATE_HELP_SNAPSHOT=1 cargo test -p colonizer-harness help_output\ngone:\n\
                 {gone:#?}\nadded:\n{added:#?}"
            );
        }
    }
}
