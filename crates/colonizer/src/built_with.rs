//! "Built with" (issue #944): the products Colonizer is made of, said out loud — the venture's own
//! entry in the Factory Zero registry (`https://factory0.ventures/stack.json`, venture FZ-006),
//! kept as a vendored copy in `built-with.json` beside this crate's `Cargo.toml`.
//!
//! The copy is the source of truth, not a cache of something fetched at runtime: the mothership
//! answers the list without a network call, an entry cannot change under a running install, and a
//! reader can see exactly what was claimed and on what date by reading one file in the repository.
//! Refreshing it is a deliberate act — copy the registry entry, commit the diff, and the product
//! says the new thing.
//!
//! Every entry carries a `status` of `live` or `planned`, and it is the whole point of the
//! module: a product the venture has chosen but not yet switched on is never rendered as though it
//! were in use. [`Status`] makes that structural — a vendored copy with a status this build does
//! not know does not parse at all, so a typo or a third status cannot leak through as "live".
//!
//! Nothing here is scoped to a token. The list is install-wide public information — it is already
//! on the venture's own page — so the route is owner-only by default (`token_scope: None`, like
//! `quota_cards`), which is what the cockpit authenticates as. There is no scoped-token rule to
//! invent, and inventing one would only narrow a list that names nothing private.

use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// The routes, and their owner-only default, registered in `features::ALL`.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "built_with",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/built-with", routing::get(list))
}

/// The role the registry gives the product that sends mail, which is what the footer below names.
#[cfg_attr(not(test), allow(dead_code))]
const EMAIL_ROLE: &str = "email";

/// Colonizer's entry in the Factory Zero registry, as vendored on the day it was retrieved.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Registry {
    /// Where the registry lives; the copy records the source it was taken from.
    pub registry: String,
    /// The registry's own id for this venture, `FZ-006`.
    pub venture: String,
    /// The venture's name as the registry spells it.
    pub venture_name: String,
    /// The venture's page on the registry's site.
    pub venture_page: String,
    /// The day the vendored copy was taken, `YYYY-MM-DD`: a list without a date cannot be said to
    /// be true of anything in particular.
    pub retrieved: String,
    /// What the venture is made of, in the registry's own order.
    pub uses: Vec<Use>,
}

/// One product in the stack.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Use {
    /// The registry's id for the product, or the vendor's own slug for a third party.
    pub id: String,
    /// `factory-zero` for a venture from the same registry, `third-party` for anything else.
    pub kind: String,
    /// The product's name, as its own site spells it.
    pub name: String,
    /// What this venture actually does with it, in a sentence — the part a reader cannot infer.
    pub note: String,
    /// The words the list puts before the name: "Built with", "Hosted on", "Email by".
    pub phrase: String,
    /// What the product is for here — `framework`, `email`, `hosting`, and so on. The footer below
    /// reads it to find the mail one, rather than matching on a product's name.
    pub role: String,
    /// Whether the venture is using it today (`live`) or has said it will (`planned`).
    pub status: Status,
    /// The product's home page, which the list links.
    pub url: String,
}

/// Whether a product is in use today or only intended.
///
/// An enum, not a string, on purpose: the registry grows a third status someday and a vendored
/// copy carrying it will fail to parse rather than render as one of these two, so a planned
/// product can never be shown as a live one by a mistyped or newly invented value.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// In use now, and said so in public.
    Live,
    /// Chosen, not yet in use: waitlist mail, a billing provider with nothing on sale, a
    /// deployment console that is not carrying a production site yet.
    Planned,
}

impl Status {
    /// The word the product shows a reader, which is the registry's own.
    pub fn word(self) -> &'static str {
        match self {
            Status::Live => "live",
            Status::Planned => "planned",
        }
    }
}

/// The vendored copy, parsed once per process.
///
/// A parse failure here is a build defect, not a runtime condition: the file is compiled into the
/// binary by `include_str!`, so it either parsed at build time in the tests below or it did not.
static REGISTRY: LazyLock<Registry> = LazyLock::new(|| match serde_json::from_str(include_str!("../built-with.json")) {
    Ok(registry) => registry,
    Err(e) => panic!("built-with.json did not parse: {e}"),
});

/// Colonizer's vendored registry entry.
pub(crate) fn registry() -> &'static Registry {
    &REGISTRY
}

/// The "Sent with Owlpost" line for a transactional email, or `None` while Owlpost is only planned.
///
/// This is the single switch. The rule is: a mail footer may only name a product the registry says
/// is `live` today, and Owlpost is `planned` — waitlist confirmation is written down, no mail is
/// sent — so this returns `None` and no message is ever stamped. When the registry entry flips to
/// `live` and the vendored copy is refreshed, the footer appears with no further code change, which
/// is the point: the claim in the footer and the claim in the list come from one file and cannot
/// disagree.
///
/// The name and the address are both read out of that entry rather than written here, so a rename
/// or a new address in the registry moves the footer with it.
///
/// Nothing calls this yet, and that is deliberate. This repository has no mail transport at all —
/// no SMTP client, no templates, no `lettre` — so there is nowhere to put a footer even if one were
/// owed. The rule is specified and tested here so that whoever adds the transport inherits the
/// "planned is not live" rule instead of re-deciding it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn email_footer() -> Option<String> {
    footer_for(&registry().uses)
}

/// The footer for a given list, so the rule is testable without editing the vendored copy.
#[cfg_attr(not(test), allow(dead_code))]
fn footer_for(uses: &[Use]) -> Option<String> {
    let mail = uses.iter().find(|u| u.role == EMAIL_ROLE)?;
    if mail.status != Status::Live {
        return None;
    }
    Some(format!("Sent with {} · {}", mail.name, mail.url))
}

/// `GET /api/built-with` — what this install is made of, as the vendored registry entry says.
pub async fn list() -> Json<&'static Registry> {
    Json(registry())
}

/// The lines `colonizer about` prints: the venture and its page, one line per product with its
/// status in words, and the source the list was taken from and the day it was taken.
pub(crate) fn lines() -> Vec<String> {
    let registry = registry();
    let mut out = vec![
        format!("Built with — {} ({})", registry.venture_name, registry.venture),
        format!("  {}", registry.venture_page),
        String::new(),
    ];
    for use_ in &registry.uses {
        out.push(format!(
            "  {} {} — {} — {}",
            use_.phrase,
            use_.name,
            use_.status.word(),
            use_.url
        ));
    }
    out.push(String::new());
    out.push(format!("From {} on {}.", registry.registry, registry.retrieved));
    out
}

/// `colonizer about`: the same list, on a terminal. Local — it reads the copy compiled into this
/// binary, so it needs no mothership and no network.
pub(crate) fn print() {
    for line in lines() {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eight products the registry's FZ-006 entry lists, so a refresh that quietly drops one
    /// is a failing test and not a shorter footer page.
    const USE_COUNT: usize = 8;

    fn use_(role: &str, status: Status) -> Use {
        Use {
            id: "test".into(),
            kind: "factory-zero".into(),
            name: "Testpost".into(),
            note: "A note.".into(),
            phrase: "Email by".into(),
            role: role.into(),
            status,
            url: "https://example.invalid/".into(),
        }
    }

    #[test]
    fn the_vendored_copy_parses_and_lists_every_use() {
        assert_eq!(
            registry().uses.len(),
            USE_COUNT,
            "the vendored copy should list {USE_COUNT} products"
        );
    }

    #[test]
    fn every_status_is_live_or_planned_and_nothing_else_parses() {
        for u in &registry().uses {
            // Exhaustive already: `Status` has these two variants and no others, and the vendored
            // copy parsed, so every status is one of them. What is left to pin is the reverse
            // direction — that a status outside the two is a parse error, not a value that would
            // render as one of them.
            assert!(
                matches!(u.status, Status::Live | Status::Planned),
                "{} has an unknown status",
                u.name
            );
        }
        let unknown = r#"{"status":"shipping-soon"}"#;
        assert!(
            serde_json::from_str::<Status>(unknown).is_err(),
            "a status outside live and planned must not parse, or it could render as one"
        );
    }

    #[test]
    fn every_entry_carries_a_phrase_a_name_a_note_and_an_https_url() {
        for u in &registry().uses {
            for (field, value) in [("phrase", &u.phrase), ("name", &u.name), ("note", &u.note), ("role", &u.role)] {
                assert!(!value.trim().is_empty(), "{} has an empty {field}", u.name);
            }
            assert!(
                u.url.starts_with("https://"),
                "{} has a url that is not https: {}",
                u.name,
                u.url
            );
        }
    }

    #[test]
    fn the_registry_and_venture_metadata_are_the_ones_the_copy_was_taken_from() {
        let registry = registry();
        assert_eq!(registry.registry, "https://factory0.ventures/stack.json");
        assert_eq!(registry.venture, "FZ-006");
        assert_eq!(registry.venture_name, "Colonizer");
        assert_eq!(registry.venture_page, "https://factory0.ventures/ventures/colonizer/");
        assert!(
            !registry.retrieved.is_empty(),
            "a list with no retrieval date is a list of claims"
        );
    }

    #[test]
    fn the_email_footer_is_absent_while_the_mail_product_is_only_planned() {
        // The vendored copy says what the registry said on the day it was taken: waitlist
        // confirmation is planned, so no mail is stamped.
        assert_eq!(email_footer(), None, "Owlpost is planned, so nothing may be stamped on mail");
    }

    #[test]
    fn the_email_footer_appears_once_the_mail_product_is_live_and_names_its_address() {
        let uses = [use_("framework", Status::Live), use_(EMAIL_ROLE, Status::Live)];
        let footer = footer_for(&uses).expect("a live mail product earns a footer");
        assert_eq!(footer, "Sent with Testpost · https://example.invalid/");
        assert!(
            footer.contains("https://example.invalid/"),
            "the footer must name where the mail came from: {footer}"
        );
    }

    #[test]
    fn no_footer_is_stamped_while_the_mail_product_is_planned_or_absent() {
        assert_eq!(footer_for(&[use_(EMAIL_ROLE, Status::Planned)]), None);
        assert_eq!(
            footer_for(&[use_("framework", Status::Live)]),
            None,
            "no mail product, no footer"
        );
        assert_eq!(footer_for(&[]), None);
    }

    #[test]
    fn the_printed_list_names_the_venture_the_statuses_and_the_source() {
        let printed = lines().join("\n");
        assert!(printed.contains("Colonizer (FZ-006)"), "{printed}");
        assert!(printed.contains("https://factory0.ventures/ventures/colonizer/"), "{printed}");
        for u in &registry().uses {
            assert!(
                printed.contains(&u.phrase) && printed.contains(&u.name),
                "{} is missing from the list",
                u.name
            );
            assert!(
                printed.contains(u.status.word()),
                "{}'s status is missing from the list",
                u.name
            );
            assert!(printed.contains(&u.url), "{}'s url is missing from the list", u.name);
        }
        let printed_lines = lines();
        let source = printed_lines.last().expect("a trailing line");
        assert!(source.contains("https://factory0.ventures/stack.json"), "{source}");
        assert!(
            source.contains(&registry().retrieved),
            "the source line should carry the day it was taken: {source}"
        );
    }
}
