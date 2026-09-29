**Log archives and fleet exports no longer carry a colony's credentials.** A session directory
holds live bearers: agentd's `vm/token`, the colony's `gateway-token`, the mesh auth key and
`vm/session.json` (whose runner env repeats the gateway bearer). The log archive tarred all of
them, so anyone holding a bundle could drive a colony that was still running or could be resumed.
These files, and anything else in a session directory named like a credential (`*token*`,
`*authkey*`, `*.key`, `*.pem`, `secrets*`), are now left out of every archive bundle and fleet
export, including the export's fallback to an older archive bundle. The fleet export also redacts
the logs it carries, and the findings ledger (`findings.jsonl`) is redacted as it is written.
([#761])
