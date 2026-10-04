# Saved secrets: `/api/secrets`

Part of the [Colonizer protocol](../protocol.md).

Every saved secret — the GitHub and Claude tokens, Claude accounts, model-provider keys, voice,
mem0 and notification keys — lives either in the **system keychain** (Keychain on macOS, the Secret
Service on Linux; service `dev.colonizer`, account = the secret's path under the config directory,
e.g. `provider-keys/zai`) or in its **0600 file** under the config directory (encrypted as the `.enc`
envelope when `COLONIZER_MASTER_KEY` is set). `<config>/secrets.json` records which, and when it last changed; it
never holds a value. At startup the mothership writes, reads back and deletes a canary item (each
call bounded at 5 s) to decide whether the keychain is usable; a headless Linux host without a
D-Bus session or with a locked keyring fails it and keeps using files.

Nothing moves on its own: a secret already on file stays there until it is moved. A secret saved
for the first time goes to the keychain when the probe passed, else to its file. Keychain items are
read once at startup and cached, so macOS asks for access then, not in the middle of a request.

- `GET /api/secrets` → `{"keychain": {available, backend, reason, checked_at}, "secrets": [{id,
  label, group: "providers"|"connections"|"integrations"|"colonies", used_by, icon, location:
  "keychain"|"file"|"env"|"unset", env, env_set, updated_at, editable, colonies: {kind:
  "gateway"|"injected"|"none", hosts}}]}`. Values are never returned. `id` is the path with `/` as
  `:` (`provider-keys:zai`). `colonies` says what a colony gets of the secret: `gateway` (model
  keys: the gateway adds them to routed requests, they never enter a microVM), `injected` (handed to
  the colony as an msb `--secret ENV@hosts`: the guest sees a placeholder and msb substitutes the
  value only on TLS to `hosts` — the Claude token for `api.anthropic.com`, the Jev key for
  `api.typesafe.ai`, and colony secrets), or `none` (the mothership's own use only).
- `GET /api/secrets/health` → the `keychain` object alone.
- `PUT /api/secrets/{id}` `{"value": "…", "location"?: "keychain"|"file"}` → the row. One line, at
  most 16 KB. Without `location` the secret stays where it is (a new one follows the rule above).
- `DELETE /api/secrets/{id}` → the row; removes the keychain item and the file.
- `POST /api/secrets/{id}/move` `{"to": "keychain"|"file"}` → the row; carries the value across and
  deletes the other copy. 409 when the keychain is unavailable or nothing is saved.
- Not editable here (`editable: false`): the cockpit API token (the CLI reads it off disk) and keys
  only an environment variable supplies (`JEV_API_KEY`).
- `DELETE /api/secrets/colony:<ENV>` removes a colony secret's value and its registry entry, and
  answers `{"id": "colony:<ENV>", "removed": true}`.
- Errors: **404** for an unknown id; **400** for a row that is not editable, a value that is empty,
  more than one line or over 16 KB, or a `to` other than `keychain`/`file`; **502** when the save
  fails; **503** while the secret store is not running.

## Colony secrets: `POST /api/secrets/colony`

Keys the operator lets colonies use, e.g. a Stripe test key for a test suite.
`{"env": "STRIPE_TEST_KEY", "hosts": ["api.stripe.com"], "scope": {"kind": "all"} | {"kind": "org",
"org": "acme"} | {"kind": "repo", "repo": "acme/web"}, "value"?: "…"}` → `{id, env, hosts, scope}`.
Adds the secret or changes its hosts, scope or value; `value` is required for a new one and omitted
keeps the saved value. 400 when:
- a new secret has no `value`, or the value is not one line of at most 16 KB;
- `env` is not `[A-Z_][A-Z0-9_]*` (at most 64), or is reserved (`PATH`, `HOME`, `GH_TOKEN`,
  `GITHUB_TOKEN`, `TYPESAFE_API_KEY`, `JEV_API_KEY`, `MEM0_API_KEY`, … and the prefixes
  `ANTHROPIC_`, `CLAUDE_`, `COLONIZER_`, `OPENAI_`, `NODE_`, `MSB_`, `LD_`);
- a host is not a public DNS name (wildcards, IP literals, ports, schemes, `localhost`, `.local`,
  `.internal` and similar are refused), or there are none or more than 10;
- the scope names no org or no `owner/name`.

`<config>/colony-secrets.json` holds names, hosts and scope only; each value is saved like any other
secret at `colony-secrets/<ENV>` (the keychain when available). At boot, every colony secret whose
scope admits the colony's repository (org and repo compare case-insensitively) and that has a value
is passed to msb as `--secret ENV@hosts`, never as a plain environment variable, and the colony's
prompt gains a `<colony-secrets>` block listing only the names and their hosts, with the instruction
not to print, log, commit or persist them. The harness log records which names a colony received.
