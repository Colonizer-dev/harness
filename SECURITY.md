# Security policy

Colonizer runs coding agents inside microVMs on your machine and holds your GitHub and model
credentials on the host. A flaw that lets a colony reach past its VM, read a host secret, or act
through the host's API is a security bug. So is anything that lets another program on the machine
or network drive the harness.

## Reporting a vulnerability

Report it privately through GitHub: **Security → Report a vulnerability** on this repository
(<https://github.com/Colonizer-dev/harness/security/advisories/new>). Please do not open a public
issue for it.

Include what you did, what you expected, what happened, and the version (`colonizer --version`).
A proof of concept helps but is not required.

## What happens next

- We acknowledge the report within 3 working days.
- We confirm or dispute it, and agree a disclosure date with you, within 10 working days.
- The fix lands with a regression test that fails without it, and ships in a release before the
  advisory is published. The advisory credits you unless you ask otherwise.

## Supported versions

Only the latest release is supported. Fixes are not backported; update with `colonizer update`.

## Scope

In scope: the harness host (`crates/colonizer`), the in-VM daemon (`crates/colonizer-agentd`), the
cockpit web UI (`web/`), the model gateway, the release artifacts and install scripts.

Out of scope: vulnerabilities in the coding agents themselves or in the model providers; they
belong with those projects. What a colony can reach from inside its VM *is* in scope.

Known, still-open weaknesses are listed in [docs/audit.md](docs/audit.md).
