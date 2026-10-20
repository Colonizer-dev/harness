# planetscale

[PlanetScale](https://planetscale.com) is the MySQL-compatible serverless database platform. The **planetscale** skillset integrates it into Colonizer as an optional, **off-by-default** downloadable set of tools and scripts. Nothing installs it until an operator asks, and downloading it does not switch it on.

## Switching it on

Settings → Skillsets lists it with a Download button. Downloading unpacks the plugin to `<data>/plugins/planetscale` and makes it an ordinary skillset; it is **not** enabled by that. Flip its switch (the Claude Code module's `plugins` setting, a comma-separated list of names, or an org's `agent.skillsets` override) and new colonies mount it read-only at `/opt/colonizer/plugins/planetscale`.

The same two endpoints graft has serve it, by name:

| | |
| --- | --- |
| Status | `GET /api/plugins/planetscale` — `{name, release, installed_release, state, bytes, total, started_at, finished_at, error}`, where `state` is `idle`, `downloading`, `unpacking`, `installed`, `failed`, `unavailable` or `local` (an operator's own `plugins/planetscale` directory is used instead) |
| Download | `POST /api/plugins/planetscale/download` starts or joins the download and returns the same object at once; 409 for `local` |
| Listing | `GET /api/plugins` carries the same object in its `downloadable` array |

All three need an owner token.

## What it ships

The skillset contains:

- **`pscale` CLI** (Apache-2.0, v0.347.0): the PlanetScale command-line tool for database management, pinned per architecture (`linux-x86_64`, `linux-aarch64`, `darwin-aarch64`). Mounted read-only at `/opt/colonizer/plugins/planetscale/bin/pscale`, it joins the colony's PATH when the skillset is on.

- **`planetscale/skills`** (MIT): a collection of verified workflows for common PlanetScale tasks. Unpacked alongside `pscale` and mounted read-only.

- **PlanetScale MCP** (`https://mcp.pscale.dev/mcp/planetscale`): a hosted tool server for when PlanetScale accepts service-token authentication there, or for operator use outside colonies (colonies have no browser, so OAuth login is not an option). The host `mcp.pscale.dev` joins the colony's egress allowlist only when the skillset is on.

- **Licenses**: MIT for skills, Apache-2.0 for the CLI.

## Its pin

`crates/colonizer/planetscale.lock` pins three rows: `pscale` for each Linux architecture and `planetscale-skills` (the repository):

```
pscale        0.347.0  linux-x86_64    archive  <sha256>  https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_linux_amd64.tar.gz
pscale        0.347.0  linux-aarch64   archive  <sha256>  https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_linux_arm64.tar.gz
pscale        0.347.0  darwin-aarch64  archive  <sha256>  https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_macOS_arm64.tar.gz
planetscale-skills  <commit>  any  source  <sha256>  https://codeload.github.com/planetscale/skills/tar.gz/<commit>
```

Colonies get the Linux build for their architecture. Minimum `pscale` version is 0.292.0, needed for `pscale agent-guide` and `pscale auth check --format json`. Nothing adopts a new pin on its own — `scripts/update-runtime-pins.mjs` runs daily, checks for updates, and opens a pull request when a pin moves. By hand:

```sh
node scripts/update-runtime-pins.mjs --check           # report what moved
node scripts/update-runtime-pins.mjs --write --summary out.md
```

## Auth: service tokens only

Colonies never run `pscale auth login`. Instead, the operator creates a PlanetScale **service token** per venture or org (PlanetScale dashboard → org Settings → Service tokens; grant only the database permissions that venture needs) and stores the credentials as colony secrets:

- `PLANETSCALE_SERVICE_TOKEN_ID` (the token's public ID)
- `PLANETSCALE_SERVICE_TOKEN` (the token's secret)

Both are scoped to the org or repo that needs them, with host `api.planetscale.com`. The colony sees only a placeholder; the mothership's microsandbox (msb) swaps in the real values on TLS connections to that host, so tokens never sit in the guest environment, logs, or pull requests. The redactor also recognises `pscale_tkn_`, `pscale_pw_`, and `pscale_oauth_` prefixes in any output.

pscale sends credentials as `Authorization: <id>:<token>`. Check that auth is working with:

```sh
pscale auth check --format json    # should report "status": "ok"
```

Commands unavailable under a service token are not retried; use the `pscale api` alternative:

- `pscale org show` → `pscale api organizations/<org> --format json` + `--org`/`PLANETSCALE_ORG`
- `pscale service-token list` → `pscale api organizations/<org>/service-tokens --format json`
- `pscale service-token show-access` → `pscale api organizations/<org>/service-tokens/<id> --format json`

## Approval gates for writes

Schema changes in PlanetScale go through **branches and deploy requests**, committed in a pull request, never directly to production. The Claude Code agent's built-in exec policy (module `modules/agents/claude-code/execpolicy.mjs`) holds any `pscale sql` or `pscale shell` command with a write role (`--role admin|writer|readwriter`, also covers `pscale password create`) for operator approval through the usual approval-hold mechanism. Commands with `--force` are also held, since `--force` can delete or drop data.

This is the same flow as any other high-risk operation: the agent proposes, the operator answers the question, and only then does the command run. A write without approval is refused.

## Inside a colony

The skillset mounts read-only, and colonies have no browser. So:

- **No OAuth login.** `pscale auth login` does not work and should not be used. Service tokens are the working path.
- **Service-token auth.** The colony's secrets are swapped in by msb on TLS to `api.planetscale.com`; the `pscale` CLI sees them in the `PLANETSCALE_SERVICE_TOKEN_ID` and `PLANETSCALE_SERVICE_TOKEN` environment variables, never in secrets files.
- **Hosted MCP for later.** The PlanetScale MCP at `https://mcp.pscale.dev` authenticates via OAuth in browsers. When PlanetScale's service-token support reaches the MCP, it will work in colonies. For now, the hosted MCP is for operator use outside colonies — the egress allowlist includes it but the colony has no way to authenticate.

## Hyperdrive pairing

Ventures running PlanetScale behind Cloudflare Hyperdrive (EarthOS is the first) have two paths:

- **In the app:** connections through Hyperdrive use a least-privilege app role; they never use the owner/admin role.
- **For migrations:** the owner/admin role is used only for schema changes — from CI or an approved colony command — and never in Hyperdrive's connection string.

This prevents accidentally opening admin access to the app layer.

## Updating planetscale

`pscale` is pinned by release tag and sha256. A new release shows the Download button again on the Skillsets row. `planetscale-skills` updates by commit: bump its entry in `planetscale.lock` and a pull request opens the update for review.

## See also

- [skill-packs.md](skill-packs.md#downloadable-skillsets) — what a downloadable skillset is, and graft's row as the worked example.
- [protocol/plugins.md](protocol/plugins.md) — plugin directories, validation, resolution.
- [protocol/secrets.md](protocol/secrets.md#colony-secrets-post-apisecretscolony) — how to set up and use colony secrets.
