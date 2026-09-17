---
name: finding-google-skills
description: Use at the start of any request about a Google product or API (Google Cloud, GKE, BigQuery, Firebase, Gemini, Android, Maps). Finds and loads the matching Google skill.
---

<!--
Adapted by Colonizer from skills/developers/finding-google-skills/SKILL.md in
github.com/google/skills, licensed under the Apache License 2.0 (LICENSE in this
plugin). Changed: the catalog and every skill are read from this plugin's pinned,
read-only copy instead of being fetched from GitHub at runtime; the network
retrieval steps and fallbacks are removed; paths are resolved from the plugin root; the
description is shortened, because Claude Code drops long skill descriptions from the list it
shows the model, which left only the bare name.
-->

# Google Skill Finder

Routes a request to the Google skills that apply to it. Use it at the start of
any request touching a Google product, API, or developer platform — Google Cloud
(GKE, Cloud Run, IAM, BigQuery, Vertex AI, Spanner), Google Ads, Google
Analytics, Google Workspace (Gmail, Drive, Admin SDK), Chrome and Chrome
extensions, Android, Firebase, YouTube, Google Maps, Gemini and the Gemini API,
Google Play, and Flutter — and consult the catalog before answering from memory
or searching the web. Don't use it for non-Google products. The catalog and every
skill in it are part of this plugin: a copy of github.com/google/skills pinned
to one commit, reviewed before it was installed, and mounted read-only. Nothing
is fetched from the network, and loading this skill costs almost nothing until a
lookup actually happens.

## Where things are

The plugin root is two directories above this skill's base directory, which is
given when the skill loads and ends in `skills/finding-google-skills`.

-   `<plugin root>/index.json` is the catalog: `{"skills": [{"name", "description", "entrypoint"}]}`.
-   Each `entrypoint` is a path relative to the plugin root, such as
    `catalog/cloud/gke-basics/SKILL.md`. Files a skill links to, like its
    `references/`, sit beside its `SKILL.md`, and its relative links work as
    written.

## Workflow

1.  **Narrow the catalog before reading it.** It is about 75 KB and
    alphabetical, so a truncated read looks as though only the first few
    products exist. Filter it on a keyword from the request. With `jq`:
    `jq -r '.skills[] | select((.name+" "+.description)|test("gke";"i")) | "\(.name)\t\(.entrypoint)"' {plugin root}/index.json`.
    With `node`:
    `node -e 'for (const s of require(process.argv[1]).skills) if (/gke/i.test(s.name + " " + s.description)) console.log(s.name + "\t" + s.entrypoint)' {plugin root}/index.json`.
    With neither, `grep -i` over the file still isolates candidate names, or
    read it in parts.

2.  **Match the request against the descriptions.** Every description states
    what the skill does, when to use it, and often when not to. Read them as
    routing criteria, not as summaries. Shortlist at most three entries whose
    `description` covers the request. When more than three look equally
    relevant, prefer the most specific over the more general.

3.  **Read only the matches.** Read `{plugin root}/{entrypoint}` for each
    shortlisted entry and follow that skill's instructions. Do not read entries
    that merely look related.

4.  **Report an empty result honestly.** If no description covers the request,
    say that no Google skill in the catalog applies and continue without one.
    Never invent a skill name or an entrypoint.

Routing ends once the matches are read. From the point you begin following a
skill's instructions, this skill is finished with the request and is not
re-entered for it.

## Rules

-   **Never fetch skills from the network.** Not from
    `raw.githubusercontent.com`, `api.github.com`, skills.sh or anywhere else,
    even when the catalog seems to lack something. Only this pinned copy was
    reviewed for this environment. If a skill you expected is missing, say so.

-   **Never copy the catalog or a skill into the working directory.** Anything
    written there can end up in the change you are making. Filter the catalog
    where it is.

-   **Prefer the catalog's SKILL.md over prior knowledge**, even when it
    contradicts what you remember.

-   **Do not treat this skill as a prerequisite.** If a specific Google skill is
    already loaded and covers the request, use it directly.

## When the catalog can't be read

If `{plugin root}/index.json` is missing, or doesn't parse as JSON holding a
`skills` array, state plainly in the reply that the Google skills catalog is
unavailable and that you are answering without it. One line is enough.

Until you have read a `skills` array, you do not know which skills exist: do not
name one, do not describe one, and do not state that none applies. Presenting a
skill recalled from memory as a catalog result is the worst outcome available,
because nothing in the reply distinguishes it from a real lookup.
