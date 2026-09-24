#!/usr/bin/env node
// Makes the CI jobs required status checks on the default branch (issue #367). The change is a
// repository ruleset, not classic branch protection: the classic PUT /repos/…/branches/main/protection
// replaces the whole protection object, so applying it from a script could quietly drop a setting
// someone set by hand, while a ruleset layers on top of whatever protection already exists.
//
//   node scripts/require-ci-checks.mjs                      dry run (the default): print the plan and the payload
//   node scripts/require-ci-checks.mjs --apply              create the ruleset, or update it in place (idempotent)
//   node scripts/require-ci-checks.mjs --repo owner/name    another repository (default Colonizer-dev/harness)
//
// The update replaces the ruleset wholesale: a rule, ref condition or bypass actor someone adds to
// "CI required on main" by hand in the UI is dropped by the next run — change it here, not in the UI.
//
// Needs gh with a token (GH_TOKEN) that administers the repository. Two settings travel with this
// one and are flipped by hand in Settings → General: "Allow auto-merge" must be on — the harness
// queues a colony pull request held for required checks with `gh pr merge --squash --auto`, which
// GitHub refuses without it — and no merge queue must be configured, because no workflow here has a
// `merge_group` trigger, so a queued pull request would wait forever.

import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

export const RULESET_NAME = 'CI required on main';
export const DEFAULT_REPO = 'Colonizer-dev/harness';

// The ci.yml job ids, which are the status-check contexts: no job there has a `name:` override or a
// matrix, and all six run on every pull_request with no path filter, so requiring them cannot block
// a pull request that never triggered them. Left out, deliberately:
// - `colony-smoke` (ci.yml): a self-hosted KVM job skipped unless vars.COLONIZER_KVM_RUNNER is set;
//   until the repository has such a runner it never reports, and a required check that never
//   reports would block every merge.
// - `vulnerabilities` and `sbom` (supply-chain.yml): a newly published advisory can turn the audit
//   red with no commit at all — the weekly scheduled run is the detection path — and an SBOM is
//   provenance evidence, not a gate. Requiring either would block merges on news, not on the change.
// - the release.yml jobs: paths-filtered or tag-only, so an ordinary pull request never sees them.
export const REQUIRED_CHECKS = ['rust', 'runner', 'scripts', 'telemetry', 'web', 'colony-report'];

// The GitHub Actions app. Pinning each context to it means only a status the real Actions run
// reported satisfies the check — another app cannot post a green status under the same name.
const GITHUB_ACTIONS_INTEGRATION_ID = 15368;

/** The POST/PUT body: the same payload creates the ruleset and updates it in place. */
export function buildRulesetPayload() {
  return {
    name: RULESET_NAME,
    target: 'branch',
    enforcement: 'active',
    // `~DEFAULT_BRANCH` follows the repository's default branch instead of hardcoding `main`.
    conditions: { ref_name: { include: ['~DEFAULT_BRANCH'] } },
    rules: [
      {
        type: 'required_status_checks',
        parameters: {
          // Not strict: a pull request need not be up to date with its base at merge time. The
          // harness already updates a behind branch from its base before it merges (validation.rs
          // `merge_decision`), and the colony PRs rebase themselves when GitHub marks them DIRTY.
          strict_required_status_checks_policy: false,
          required_status_checks: REQUIRED_CHECKS.map((context) => ({
            context,
            integration_id: GITHUB_ACTIONS_INTEGRATION_ID,
          })),
        },
      },
    ],
  };
}

/** What a list of the repository's rulesets leaves to do: create one, or update it by id. */
export function planFor(rulesets, name = RULESET_NAME) {
  const found = (Array.isArray(rulesets) ? rulesets : []).find((ruleset) => ruleset?.name === name);
  return found ? { action: 'update', id: found.id } : { action: 'create' };
}

const USAGE = `usage: node scripts/require-ci-checks.mjs [--apply] [--repo owner/name]

  --apply             send the payload with gh api (the default is a dry run that only prints it)
  --repo owner/name   which repository to read and write (default ${DEFAULT_REPO})`;

/** {repo, apply} from argv, or null (after printing the usage) when the arguments do not parse. */
export function parseArgs(argv) {
  const out = { repo: DEFAULT_REPO, apply: false };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--apply') out.apply = true;
    else if (argv[i] === '--repo') {
      const value = argv[i + 1];
      if (!value || !/^[^/\s]+\/[^/\s]+$/.test(value)) {
        console.error(`--repo wants an owner/name pair, not ${JSON.stringify(value ?? 'nothing')}`);
        console.error(USAGE);
        return null;
      }
      out.repo = value;
      i++;
    } else if (argv[i] === '--help' || argv[i] === '-h') {
      console.log(USAGE);
      return null;
    } else {
      console.error(`unknown argument ${JSON.stringify(argv[i])}`);
      console.error(USAGE);
      return null;
    }
  }
  return out;
}

/** One gh api call; `body`, when given, is sent as the request body via `--input -`. */
function gh(method, path, body) {
  const args = ['api', '--method', method, path];
  if (body !== undefined) args.push('--input', '-');
  // --paginate matters only for the list: gh merges REST array pages into one, so a repository with
  // more rulesets than one page still finds this script's ruleset by name.
  else args.push('--paginate');
  const run = spawnSync('gh', args, { encoding: 'utf8', input: body ?? '' });
  if (run.error) {
    console.error(`cannot run gh: ${run.error.message}`);
    process.exit(1);
  }
  if (run.status !== 0) {
    console.error((run.stderr || run.stdout).trim() || `gh api ${path} failed`);
    process.exit(1);
  }
  return run.stdout;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (!options) process.exit(2);

  const existing = JSON.parse(gh('GET', `repos/${options.repo}/rulesets`));
  const plan = planFor(existing);
  const payload = buildRulesetPayload();
  const where = plan.action === 'update' ? `update ruleset #${plan.id} on` : 'create a ruleset on';
  console.log(`Plan: ${where} ${options.repo} — the payload, exactly as it would be sent:`);
  console.log(JSON.stringify(payload, null, 2));
  if (!options.apply) {
    console.log('\nDry run: nothing was sent. Pass --apply to create or update the ruleset.');
    return;
  }

  const path = plan.action === 'update' ? `repos/${options.repo}/rulesets/${plan.id}` : `repos/${options.repo}/rulesets`;
  const response = JSON.parse(gh(plan.action === 'update' ? 'PUT' : 'POST', path, JSON.stringify(payload)));
  console.log(`\nApplied as ruleset #${response.id} (${response.enforcement}): ${response._links?.html?.href ?? ''}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) main();
