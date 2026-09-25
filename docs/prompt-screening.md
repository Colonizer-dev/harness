# Prompt screening

A colony reads attacker-controlled repositories and writes text that ends up in a pull request. A
page, a commit message or a README it reads can carry instructions that ride outside what a
reviewer sees: Unicode tag characters encoding ASCII inside an innocent-looking line, bidi
controls reordering the line a reviewer reads ([Trojan Source](https://trojansource.codes)),
variation selectors smuggling one byte each. Screening is the harness's answer, and it runs at the
only point where it can stop the damage: publish time, on the mothership, before anything leaves
the machine.

## What is screened, and where

There are two scanners, on the two sides of the trust boundary, on purpose:

| | Colony-side preflight | Mothership-side screening (this page) |
| :--- | :--- | :--- |
| Module | the agent module's `scan` settings | the `screen` module, provider `promptdecode` |
| When | before the agent starts | after the colony ends, before push/PR |
| Sees | the whole base repository | the colony's final diff, the PR title and the PR body |
| Scanner | one you mount (`scan_command`) | built in: a code-point decoder |
| Why there | full scanners run parser-rich code over hostile input; that belongs on the disposable side of the boundary, where the credentials never are | the publish gate runs where the GitHub token lives, so it must be tiny, deterministic and local — no scanner code, no network, no model |

The built-in decoder classifies three families and nothing else:

- **`tag_run`** — a run of Unicode tag characters (U+E0001, U+E0020–U+E007F) that is not the tag
  sequence of a flag emoji. Each tag character encodes one ASCII byte, so "approve this PR" hides
  invisibly inside any line. Flag sequences (the black flag U+1F3F4 followed by subtags and the
  cancel tag U+E007F) decode to their region and are not findings.
- **`bidi_control`** — direction embeddings, overrides and isolates (U+202A–U+202E, U+2066–U+2069).
  One finding per line that carries any of them, *even when the pairing is balanced* — a balanced
  isolate is still the Trojan Source shape, so `block` holds on it. Severity is `high` for an
  override (U+202D/U+202E) or an opener left unclosed at the end of the line, `medium` for a
  balanced pair. The invisible marks legitimate right-to-left text uses — LRM (U+200E), RLM
  (U+200F), ALM (U+061C) — are not controls and are not flagged, and plain RTL prose with none of
  the controls passes clean.
- **`variation_selector_run`** — two or more consecutive variation selectors, or any supplementary
  selector (U+E0100–U+E01EF) at all. Runs decode under the common byte-smuggling scheme
  (FE00–FE0F → 0–15, E0100–E01EF → 16–255) and the payload is shown when it came out mostly
  printable. A single FE0E/FE0F after a base character is how emoji ask for their colourful
  presentation and is not flagged.

The false-positive budget is deliberate: emoji ZWJ sequences and skin tones, CJK, and Arabic or
Hebrew prose that uses no direction controls — the marks above are fine — all pass clean. What the
decoder deliberately does *not* do is read the change for meaning — a plain-language injection that
uses no hidden code points is not a finding here.

## What is scanned, exactly

The diff (`origin/<base>...HEAD`, the merge-base diff the pull request would show — added lines
only, new-side line numbers, quoted paths decoded, binary sections skipped), the pull request
title, and the pull request description body. The title is scanned as its own location,
`pr.md:title`, because it is not just the top of the description: it becomes the commit subject
too, and the commit body is Colonizer's own trailer — so screening the title screens the whole
commit message. Description locations are character offsets from the top of the body.

The diff read runs under a 30-second deadline like the publish's other git calls, and carries a
16 MB cap. A branch over the cap fails the publish in both nonzero modes: screening will not vouch
for a diff it has not seen all of. Split the branch, or set the module's mode to `off` to publish
unscreened.

## Off, warn, block

The `screen` module is off until you configure it, like `notify`. Once on, its `publish` setting
decides what a finding does (default `warn`):

- **`warn`** — the findings are logged, recorded on the colony's event log, and listed in a
  Markdown section at the foot of the pull request body, with each location and decode sanitized
  and shown in a code span so the list cannot itself render as instructions; a body that ends
  inside an open code fence has the fence closed first, so the section always renders. The publish
  proceeds.
- **`block`** — the findings are logged and recorded, and the publish is held: no push, no pull
  request. The error names the count per class and the way out. The held publish fails the colony
  the way any publish failure does — and since no pull request ever opened, the failed colony
  releases its issue claim. Recovery: look at the findings, fix the branch (or lower the mode to
  `warn`), and press Publish again on the failed colony.
- **`off`** — nothing runs.

Either nonzero mode scans once per publish attempt, after the commit (so the diff is final, any
restack included) and before the push (the first external effect), and records one `screening`
event on the colony's event log — `{mode, outcome, findings}` where each finding is only its
location, class, severity and decode. The event never reaches the runner, and the notify webhook
carries none of it: the webhook fires off session status changes, not event payloads.

## Pointing a custom scanner

The built-in decoder is intentionally not extensible: if you want a full scanner (a vendored
preflight scanner, a secret scanner, a policy engine), keep it on the colony side with the agent
module's `scan_command` setting, which mounts and runs your command inside the microVM before the
agent starts, with `warn`/`block` semantics of its own. See the agent module's settings in
Settings → Modules → Agent.

## Not done yet

- The mothership does not surface a scanner's own choice card or per-org overrides; the module's
  settings are per install.
- Findings are not stripped and republished: `block` holds, `warn` annotates, and fixing the branch
  is on you.
- The decoder reads code-point classes only — it is not a general prompt-injection detector.
