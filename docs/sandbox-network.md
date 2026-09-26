# Sandbox network policy

This page states what a colony's network policy allows and denies. It covers microsandbox (`msb`)
0.6.18 as the harness drives it at commit `b60fe59`. Every upstream claim cites the v0.6.18 tag,
commit [`fa3e439`][tag]. [#303](https://github.com/Colonizer-dev/harness/issues/303) and
[#304](https://github.com/Colonizer-dev/harness/issues/304) build on it.

- **Provenance.** `vendor/vendor.lock:5-7` pins prebuilt v0.6.18 release tarballs by sha256, not
  source. Nobody has verified that those binaries were built from `fa3e439`.
- **Which `msb` runs.** `COLONIZER_MSB` wins, then the vendored binary, then a host install, then
  `msb` on `PATH` (`crates/colonizer/src/config.rs:52-58`). This page describes 0.6.18 only.
- **Verified or inferred.** Claims read from source are stated plainly. Claims reasoned from source
  but not tested on a running colony are marked **(inferred)**.

## How the harness passes network flags

- **Boot only.** `crates/colonizer/src/sandbox.rs:68-76` passes one `--net` with the profiles joined
  by `,` (open mode), `--net-default-egress deny` when the mode names no profile, and one
  `--net-rule` per rule. They go only to the two `msb run` invocations — detached with `--replace`
  (`crates/colonizer/src/sandbox.rs:49`), called from `crates/colonizer/src/boot.rs:1018`, and the
  attached one-shot (`crates/colonizer/src/sandbox.rs:96`), called from
  `crates/colonizer/src/verify.rs:186`. The other `msb` calls (`rm`, `ls`, `image list`, `pull`,
  `--version`) take no network flags (`crates/colonizer/src/sandbox.rs:87` and `:142-196`,
  `crates/colonizer/src/status.rs`).
- **No other policy flags.** The harness passes no `--dns-nameserver`,
  `--no-dns-rebind-protection`, pool or `--deployment-profile` flag. Besides `--net` and
  `--net-rule`, the flags that change networking are the `-p` publish in risk 5, the Claude
  credential's `--secret`, and, in allowlist mode, `--net-default-egress deny`
  (`crates/colonizer/src/sandbox.rs:62-79`).
- **Single-tenant by default.** Without `--deployment-profile` the CLI leaves the default
  ([`common.rs:1342-1344`][cli-dp]), which is `SingleTenant` ([`domain.rs:247-254`][dp-default]).
  The multi-tenant floor ([`network.rs:185-186`][dp-enforce], [`network.rs:294-301`][dp-floor])
  then does not apply, unless the host overrides the profile.
- **Host override (inferred).** A `deployment_profile` in microsandbox's host-wide config overrides
  the profile a sandbox asks for, on create and on restart ([`config/mod.rs:151-161`][dp-config],
  [`local/mod.rs:231-260`][dp-apply], [`create.rs:104`][dp-create],
  [`sandbox/mod.rs:163`][dp-restart]). That file is `~/.microsandbox/config.json`, unless
  `MSB_CONFIG_PATH` or `MSB_HOME` points elsewhere ([`config/mod.rs:863-871`][config-path],
  [`utils/lib/lib.rs:169-176`][msb-home]). The harness does not isolate it: the only variable it
  sets on `msb` is the secret's (`crates/colonizer/src/sandbox.rs:66`). If the file says
  `multi-tenant`, the floor is `from_profiles([Public])`, checked alongside the colony's policy
  ([`poll.rs:395-399`][poll-floor]). That blocks the scoped Host allows, so the gateway and
  Headscale become unreachable, and published ports are dropped ([`network.rs:523-583`][dp-ports]).
  The result is stricter: colonies break rather than gain reach.
- **Out of scope.** The Claude credential's `--secret`, passed when the agent needs Claude
  (`crates/colonizer/src/boot.rs:880-884`), is not covered here. The egress policy is built from
  `--net`, `--net-rule` and, in allowlist mode, `--net-default-egress` alone
  ([`common.rs:848-967`][cli-netpolicy]).
- **TLS interception.** A `--secret` also turns on TLS interception
  ([`common.rs:2369-2380`][cli-secret], [`builder.rs:870-887`][secret-tls]), by default on TCP 443
  with UDP to that port dropped ([`domain.rs:2464-2479`][tls-defaults],
  [`domain.rs:2591-2593`][tls-ports]). This changes TCP 853 and UDP 443 handling below.

## Which colonies get which profile

In the default `open` egress mode every colony runs with the `public` profile and nothing more.
Host loopback is reached through explicit, port-scoped `--net-rule` allows, never the broad `host`
profile. Handing a colony the `host` profile would open **every** host-loopback port to the
untrusted agent — including the cockpit control API on `127.0.0.1:7878` — so the harness no longer
does ([#375](https://github.com/Colonizer-dev/harness/issues/375)); it opens only the two ports a
colony is meant to reach. Allowlist mode ([#303](https://github.com/Colonizer-dev/harness/issues/303),
see [Egress policy](#egress-policy-303) below) names no profile at all.

- **Mesh on** is `modules.mesh_enabled()` and the vendored `headscale`, `tailscale` and `tailscaled`
  binaries present. It adds an `allow@host:tcp:<control>` rule for the headscale control port and
  the WireGuard rules (`crates/colonizer/src/boot.rs`).
- **Any provider** adds an `allow@host:tcp:<gateway>` rule for the provider gateway port. There is
  one route per provider in `providers.json`, whether or not the colony uses it
  (`crates/colonizer/src/providers.rs`).

The table is the harness-owned part of the rules; every boot then adds the DNS allow and the
always-blocked deny set of the [egress policy](#egress-policy-303) behind them.

| Mesh | Providers configured | `--net` | `--net-rule` (harness-owned) |
| :--- | :--- | :--- | :--- |
| On | Any | `public` | `allow@host:tcp:<control>`, one WireGuard rule per host IPv4, `allow@host:tcp:<gateway>` |
| Off | One or more | `public` | `allow@host:tcp:<gateway>` |
| Off | None | `public` | None |

Explicit rules are matched before the profile rules, and the profile rules allow nothing on the
Host group, so the default deny closes every other host-loopback port (cockpit API, headscale
metrics/gRPC, and any other service on loopback). The two ports that are opened:

- **Provider gateway.** It binds `127.0.0.1:41750` by default (`COLONIZER_GATEWAY_BIND`,
  parsed once as an IP:port socket address; a malformed value — a hostname included — refuses
  startup rather than falling back, `crates/colonizer/src/config.rs:65`). The guest gets
  `http://host.microsandbox.internal:<gateway port>/providers/<id>`
  (`crates/colonizer/src/providers.rs:343-357`) in `COLONIZER_MODEL_ROUTES`
  (`crates/colonizer/src/sessions.rs:1805-1810`).
- **Headscale control.** It listens on `127.0.0.1:<control>` (`crates/colonizer/src/mesh.rs:246`),
  default 41740 (`crates/colonizer/src/modules.rs:176`). The guest logs in to
  `http://host.microsandbox.internal:<control>` (`crates/colonizer/src/mesh.rs:115-116`).

The WireGuard rules (`crates/colonizer/src/mesh.rs:424-450`) are `allow@<ip>:udp:<udp_port>`, with
`udp_port` defaulting to 41743 (`crates/colonizer/src/modules.rs:177`), the port the harness node
listens on (`crates/colonizer/src/mesh.rs:188-189`).

- **Addresses.** One rule for every IPv4 that `ip -4 -o addr show scope global` lists. If that
  yields nothing, every `inet` address `ifconfig` lists is used instead. Both skip `127.*`,
  `tailscale*` and `utun*` (`crates/colonizer/src/mesh.rs:496-557`). This is one rule per address,
  not one rule overall.
- **Order.** Explicit rules go before the profile rules ([`common.rs:920-922`][cli-order]). Profile
  rules only allow, so these rules open that UDP port even on private-range host addresses, which
  `public` leaves to the default deny.
- **Read once.** The list is taken at each boot (`crates/colonizer/src/boot.rs:931`) and fixed
  for the life of the microVM (see [Runtime changes](#runtime-changes)).

## How microsandbox enforces policy

### Implementation

- **Host-side.** The network is a smoltcp userspace stack in the host `msb` process, behind libkrun
  virtio-net ([`backend.rs:1-10`][backend], [`poll.rs:1-6`][poll-hdr]). Enforcement happens
  outside the guest.
- **TCP** is checked at SYN ([`poll.rs:360-411`][poll-syn]). It is checked again, after the guest's
  connection is accepted but before the upstream dial, only when the policy has domain rules
  ([`tcp/proxy.rs:195-216`][tcp-recheck]), which the harness does not pass. A TLS-intercepted port
  is also checked against the SNI ([`tls/proxy.rs:184-190`][tls-recheck]).
- **UDP** is checked per datagram ([`poll.rs:764-772`][udp-check]).
- **ICMP** is echo only and policy-checked ([`icmp/relay.rs:1-8`][icmp-hdr],
  [`icmp/relay.rs:173-184`][icmp-check]).

### Rule order and defaults

- **Profiles.** `public`, `private` and `host` compose; `all` and `none` are terminal and cannot be
  combined with them ([`common.rs:342-358`][cli-flag], [`common.rs:871-917`][cli-profiles],
  [`types.rs:69-78`][profile-enum]). The harness passes only `public`, plus explicit rules.
- **Expansion.** Any profile prepends one DNS rule: egress to the gateway on UDP and TCP port 53
  ([`types.rs:827-835`][allow-dns]). Each profile then adds one `allow egress` rule for its group,
  with any protocol and any port ([`types.rs:295-331`][from-profiles],
  [`types.rs:769-771`][allow-egress]).
- **Defaults.** Egress defaults to deny and ingress to allow
  ([`types.rs:326-330`][profile-defaults]).
- **Evaluation.** First match wins, per direction, then the default ([`types.rs:1-20`][types-hdr],
  [`types.rs:448-491`][egress-walk], [`types.rs:545-562`][ingress]). Rule grammar is at
  [`net_rule.rs:6-17`][grammar].

### Destination groups

Each address falls into one group, tested in this order ([`destination.rs:40-60`][classify]). Only
IPv4-mapped `::ffff:a.b.c.d` is unwrapped first ([`addr.rs:14-22`][normalize]). `0.0.0.0` and `::`
match no group at all.

| Order | Group | Ranges | Source |
| :--- | :--- | :--- | :--- |
| 1 | Host | This sandbox's gateway IPv4 and IPv6 | [`destination.rs:65-70`][host-match] |
| 2 | Metadata | `169.254.169.254` (IPv4 only) | [`destination.rs:115-123`][metadata] |
| 3 | Loopback | `127.0.0.0/8`, `::1` | [`destination.rs:72-77`][loopback] |
| 4 | Private | `10/8`, `172.16/12`, `192.168/16`, `100.64/10`, `fc00::/7` | [`destination.rs:79-98`][private] |
| 5 | LinkLocal | `169.254/16`, `fe80::/10` | [`destination.rs:100-113`][link-local] |
| 6 | Multicast | `224/4`, `ff00::/8` | [`destination.rs:125-130`][multicast] |
| 7 | Public | Everything else | [`destination.rs:57-58`][classify-public] |

### `public`

- **Allows** every protocol and port to the Public group, plus DNS to the gateway.
- **Denies** private ranges, link-local, the metadata address, loopback and multicast. No rule
  allows those groups, so the default deny applies.
- **Host.** The gateway is its own group, so under `public` alone only the port 53 rule reaches it
  ([`types.rs:827-835`][allow-dns]). Host loopback is not reachable.

### `host`

The harness no longer passes this profile (see [Which colonies get which
profile](#which-colonies-get-which-profile) and
[#375](https://github.com/Colonizer-dev/harness/issues/375)); it is documented here because the
`Host` destination group is still used by the scoped `allow@host:tcp:<port>` rules the harness does
pass, which reach the same gateway addresses on the two allowed ports only.

- **Allows** the Host group on every protocol and port ([`types.rs:316-323`][profile-groups]). It
  adds to `public`. Host is classified before Private, so the gateway is Host even though the
  default IPv4 pool is `172.16.0.0/12` ([`destination.rs:40-60`][classify]).
- **Gateway addresses.** Each sandbox gets a `/30` from `172.16.0.0/12` and a `/64` from
  `fd42:6d73:62::/48` ([`network.rs:601-656`][pools], [`common.rs:377-385`][cli-pools]). A family
  exists only when the host has a route for it ([`network.rs:206-229`][families]).
- **Name.** `host.microsandbox.internal` ([`lib.rs:37`][alias]) is written to the guest's
  `/etc/hosts` (msb agentd [`network.rs:106-115`][hosts]) and answered by the DNS forwarder
  ([`forwarder.rs:313-320`][fwd-alias]).
- **Loopback rewrite.** TCP to the gateway IPv4 is dialled to host `127.0.0.1`, falling back to
  `::1`; TCP to the gateway IPv6 goes to `::1` first, then `127.0.0.1`
  ([`poll.rs:817-835`][tcp-host]). UDP to a gateway goes to host loopback of the same family
  ([`poll.rs:837-856`][udp-host]).
- **Net effect (inferred).** Every TCP and UDP port on host loopback is reachable at the network
  level, other than the DNS ports below and, with TLS interception on, UDP 443
  ([`poll.rs:743-749`][quic-block]).

### DNS

- **Resolver.** The guest's `resolv.conf` names the gateway ([`network.rs:410-432`][bootstrap],
  msb agentd [`network.rs:568-591`][resolv]).
- **Upstream.** The forwarder uses the host's resolvers: `/etc/resolv.conf` on Linux, the
  SystemConfiguration store first on macOS ([`nameserver/mod.rs:1-14`][ns-doc],
  [`nameserver/mod.rs:77-84`][ns-read]).
- **Name policy** is checked first; a denied name gets NXDOMAIN
  ([`forwarder.rs:289-301`][fwd-policy]). The profile DNS rule matches every query over UDP or TCP
  port 53 ([`types.rs:600-626`][dns-query]). The harness passes no name rules, so policy refuses no
  name.
- **Any resolver IP.** UDP/53 and TCP/53 to any address go to the forwarder
  ([`poll.rs:1108-1113`][udp53], [`poll.rs:551-576`][tcp53]). The TCP SYN skips the egress check
  ([`poll.rs:362-366`][tcp53-syn]). A query aimed at a non-gateway resolver is then checked against
  egress policy for that IP ([`forwarder.rs:797-828`][fwd-upstream]).
- **Other DNS ports.** UDP 853, 5353, 5355 and 137 are dropped ([`ports.rs:72-78`][dns-ports],
  [`poll.rs:751-760`][udp-altdns]). TCP 853 is refused unless TLS interception is on; then it
  skips the egress check and goes to the forwarder like TCP/53 ([`poll.rs:375-382`][dot-syn],
  [`poll.rs:579-600`][dot-proxy]).
- **DNS over TLS (inferred).** The forwarder checks such a query against policy as TCP 853
  ([`transport.rs:37-53`][dot-policy], [`forwarder.rs:834-846`][dns-action]). The profile DNS rule
  covers port 53 only ([`types.rs:827-835`][allow-dns]), and no colony rule allows the Host group on
  853, so the query gets NXDOMAIN ([`forwarder.rs:289-301`][fwd-policy]). Untested.
- **Rebind protection** is on by default ([`config/types.rs:255-263`][rebind-default]). An A or AAAA
  answer in the block lists gets NXDOMAIN unless a rule with no port filter, profile rules
  included, allows that address ([`forwarder.rs:347-376`][fwd-rebind],
  [`forwarder.rs:759-776`][rebind-allow], [`types.rs:371-393`][explicit-egress],
  [`filter.rs:12-53`][rebind-lists]). The lists cover `64:ff9b:1::/48` but not `64:ff9b::/96`.
- **Rebind gaps (inferred).** Under `public`, a listed range that the classifier puts in Public,
  such as `198.18/15` or `64:ff9b:1::/48`, is allowed by the profile rule and so still passes.
  Under the `host` profile, so does the gateway address; the harness's rules all name a port, so
  they exempt nothing. Rebind protection filters answers only; a direct IP connection never passes
  through it.

### Runtime changes

Rules cannot change while a colony runs.

- **Read once.** The policy is cloned at start ([`network.rs:326`][policy-read]) and held in an
  immutable `Arc` by the poll loop ([`poll.rs:263`][poll-arc]).
- **No modify path.** `SandboxModificationPatch` has no network field ([`modify.rs:26-79`][modify]).
- **So** a change needs a recreate. The harness recreates on every boot with `--replace`. The
  egress policy rides this: a settings change reaches a colony at its next boot or on stop plus
  Resume, which re-resolves global and org settings. No live allow/block/reset API is offered.

Composable profiles arrived in v0.6.7 ([`2026-07-24.mdx:9-18`][changelog]). The upstream changelog
has no entry for 0.6.17 or 0.6.18; its latest lists v0.6.16 ([`2026-08-28.mdx:8`][changelog-last]).

## Colony secrets and the fence

A colony secret (`POST /api/secrets/colony`, see `protocol.md`) names the hosts its value is for,
and boots as msb `--secret ENV@hosts`: msb swaps the placeholder for the value only on TLS to those
hosts. The hosts must be public DNS names, and the `public` profile already reaches the Public
group, so in the default `open` egress mode no extra `--net-rule` is added for them (allowlist mode
adds an explicit `allow@<host>:tcp:443`, see [Egress policy](#egress-policy-303) below). Private, loopback and link-local destinations
stay behind the default deny, which is why the API refuses `localhost`, IP literals and internal
names such as `*.internal` or `*.local` rather than accepting a host the colony could never reach.

## Egress policy (#303)

The fence above now carries a configurable policy on top
([#303](https://github.com/Colonizer-dev/harness/issues/303)), built in
`crates/colonizer/src/egress.rs`. There are two classes of traffic in a colony — the agent's own
model traffic (the provider gateway, the TLS-edge secret hosts) and everything else the VM does:
package managers, curls, the agent's GitHub calls. Both share one fence, and that is the honest
state of this slice: the agent and every tool in the guest run as root, so there is no in-guest
separation that could scope the policy to one class. Enforcement lives entirely in msb's
userspace network stack on the host; the policy below applies to the whole VM — package managers
included — and nothing in the guest can see, evade or tell it apart from any other traffic.
Per-process separation is what a later in-guest hardening slice would add; until then
allowlist mode means "this VM may reach", not "the agent may reach".

### What is always blocked

Every boot compiles a fixed deny set ahead of every configured rule (`egress.rs` `ALWAYS_BLOCKED`):

| Rules | Close |
| :--- | :--- |
| `deny@meta` | `169.254.169.254`, cloud metadata |
| `deny@private` | RFC 1918, CGNAT `100.64.0.0/10`, ULA `fc00::/7` ([`destination.rs:79-98`][private]) |
| `deny@loopback` | `127.0.0.0/8`, `::1` |
| `deny@link-local` | `169.254.0.0/16`, `fe80::/10` |
| `deny@multicast` | `224.0.0.0/4`, `ff00::/8` |
| `deny@host` | the sandbox's own gateway IPs, every port but the harness's allows |
| `deny@169.254.169.254` | restates `meta`, so the record reads whole |
| `deny@0.0.0.0/8`, `100.64.0.0/10`, `192.0.0.0/24`, `198.18.0.0/15`, `240.0.0.0/4`, `[fc00::/7]`, `[fec0::/10]`, `[64:ff9b::/96]`, `[64:ff9b:1::/48]`, `[2002::/16]` | the classifier gaps of [Open risks](#open-risks) item 2 — addresses `destination.rs` puts in Public today — plus restatements of `private` parts, so a classifier change cannot quietly reopen them |

msb evaluates rules first-match-wins per direction ([`types.rs:338-352`][egress-eval]). The
compiled order is fixed (`egress::compile`): one `allow@dns`, the harness's port-scoped
infrastructure allows, this deny set, the operator's blocks, the operator's allows. `allow@dns`
comes first because a DNS query is itself evaluated against Host-group rules
([`types.rs:589-626`][dns-eval]), so a bare `deny@host` behind it would otherwise take every
lookup down with the rest. Only the harness's own rules precede the deny set, configured allows
compile last, and msb's policy cannot be changed on a running sandbox (see
[Runtime changes](#runtime-changes)) — so no configuration, at either level or in a hand-edited
file, can reopen the set. A deterministic fuzz in `egress.rs` compiles hundreds of random policies
against that invariant.

### What is allowed by construction

- The WireGuard direct path (UDP) and the Headscale control port (TCP) when the mesh is on, and
  the provider gateway port (TCP) when routes exist — as above.
- Gateway port 53 (`allow@dns`): msb's own forwarder still answers, and the guest's tailscale runs
  with `--accept-dns=false` precisely so secret injection keeps using it.
- In allowlist mode, the TLS-edge secret hosts on TCP 443 — a colony with an injected credential
  that could not reach its host would only look fenced. In open mode the `public` profile already
  covers them.

### Modes and settings

The sandbox module gains three settings, validated at save time: `egress` (`open` or `allowlist`,
default `open`), `egress_allow` and `egress_block` — comma-separated `host[:port]` entries. A host
is an FQDN (lowercase, at least two labels; a leading `*.` compiles to a suffix rule), an IPv4, a
bracketed IPv6, or a CIDR of either (bracketed for IPv6). `:port` is TCP 1-65535; `:0` or no port
means every port. Single-label names are refused: in msb's grammar those words are destination
groups, not hostnames. An org may pin its own `mode` and add to both lists in its settings
(`orgs.json`, `egress`), never remove from them — resolution (`egress::resolve`) takes the org's
mode when it set one and unions the lists, and drops entries a hand-edited file let in that do not
parse: an invalid entry can only shrink a colony's reach, never widen it.

| Mode | Flags | Behaviour |
| :--- | :--- | :--- |
| `open` (default) | `--net public` | The profile fence above, with the deny set behind the harness's allows |
| `allowlist` | `--net-default-egress deny`, no `--net` | No profile allow at all; egress falls to the default deny unless a rule above or an allow-list entry allows it |

`--net none` looks like the allowlist encoding but is not one: `NetworkPolicy::none()` denies
ingress too ([`types.rs:272-279`][policy-none]), and the mesh-off published port needs ingress.
`--net-default-egress deny` with no `--net` lands as `{ default_egress: deny, default_ingress:
allow, rules: <the --net-rule tokens> }` with no profile rules
([`common.rs:848-967`][cli-netpolicy]).

Each boot records what it resolved — mode, allow, block, sources, always_blocked, rules,
profiles, `applied_at` — at `<session dir>/egress.json`, and `GET /api/sessions/{id}/egress`
serves it (404 for a colony that last booted before this existed), so the fleet view can answer
what a colony could reach without reading its boot log.

### Runtime changes under the policy

Unchanged by this slice: msb's policy is fixed per sandbox (see [Runtime changes](#runtime-changes)),
and no live allow/block/reset API is offered. A policy change reaches a colony when it next boots —
every boot runs `--replace` — or on stop plus Resume, which re-resolves global and org settings.
Because enforcement lives in msb on the host and not in the mesh, applying a policy does not depend
on the mesh being up: a wedged mesh (#167) delays the agent, not the fence.

## Host-loopback listeners

A colony's profile is `public` alone (`colony_network` in `crates/colonizer/src/boot.rs`), or no
profile at all in allowlist mode. Besides the
profile's port-53 DNS rule, which the forwarder answers ([`types.rs:827-835`][allow-dns]), its only
Host-group allows are the Headscale control port when the mesh is on
(`crates/colonizer/src/boot.rs:178`) and the gateway port when providers are configured
(`crates/colonizer/src/sessions.rs:1907-1916`). A `host` rule names the Host group
([`net_rule.rs:563-574`][rule-host]), so it covers the gateway's IPv4 and IPv6, and TCP to either is
dialled to host loopback ([`poll.rs:817-835`][tcp-host]). Every other Host port falls to the default
deny. The table lists what the harness host has on loopback, at default ports. Its `.rs` paths are
under `crates/colonizer/src/` unless given in full.

| Port | Default bind | Serves | Auth | Reachable from a colony |
| :--- | :--- | :--- | :--- | :--- |
| 7878 | `127.0.0.1`, `COLONIZER_BIND` (`config.rs:44`) | Cockpit: every `/api/*` route, including the session terminal WebSocket, and the web UI (`api_routes` and `router` in `server.rs`) | Per-install API token in `Authorization: Bearer` or the `colonizer_token` cookie (`auth.rs`). `host_guard` checks `Host`, and since #375 rejects a cookie-authenticated write or upgrade whose `Origin` is missing or does not match (`host_guard` in `server.rs`) | Default deny since #375 |
| 41750 | `127.0.0.1`, `COLONIZER_GATEWAY_BIND` (`config.rs:65`) | Provider gateway, `/providers/{id}/{*path}` (`gateway.rs:545-550`) | Per-colony token in `x-colonizer-colony`, matched against live colonies (`gateway.rs:34`, `gateway.rs:526-538`, `gateway.rs:948-956`) | Allowed when providers are configured |
| 41740 | `127.0.0.1`, mesh `control_port` (`mesh.rs:246`, `modules.rs:176`) | Headscale control server | Joining needs the pre-auth key minted per VM (`mesh.rs:352-362`); other routes not verified | Allowed when the mesh is on |
| 41741 | `127.0.0.1`, control port + 1 (`mesh.rs:247`, `mesh.rs:291`) | Headscale metrics | None set by the harness; not verified | Default deny since #375 |
| 41742 | `127.0.0.1`, control port + 2 (`mesh.rs:248-249`, `mesh.rs:292`) | Headscale gRPC | `grpc_allow_insecure: false` and no TLS configured; not verified | Default deny since #375 |
| 41744 | `127.0.0.1`, mesh `socks_port` (`mesh.rs:190-191`, `modules.rs:178`) | Harness `tailscaled` SOCKS5, which the harness uses to dial colonies (`mesh.rs:418-422`) | None (tailscale v1.102.4 [`proxy.go:98-101`][ts-socks-server], [`socks5.go:158-172`][ts-socks-auth]) | Default deny since #375 |
| Random, per colony (mesh off) | `127.0.0.1`, published to guest port 7070 (`sandbox.rs:77-79`, `boot.rs:941`) | Another colony's `colonizer-agentd`: health, events, PTY, shutdown | Per-colony bearer token (`sessions.rs:1680`, `crates/colonizer-agentd/src/main.rs:176-188`) | Default deny since #375 |
| Any | The operator's, such as the `local` provider preset's `127.0.0.1:8080` (`web/src/components/SettingsDialog.tsx:1971`) | Anything else on host loopback | Its own | Default deny since #375; the gateway still proxies to a configured provider |

Still open, all **(inferred)** and untested:

- **Gateway bind.** The gateway allow is built from the configured socket address
  (`colony_network` in `crates/colonizer/src/sessions.rs`), not from the listener that bound; a
  bind failure is only logged (`serve` in `crates/colonizer/src/server.rs`). If the gateway fails to
  bind, whatever else holds that port is reachable.
- **IPv6 loopback.** TCP to the gateway's IPv6 address is dialled to host `::1` first (see
  [`host`](#host)), so an unrelated service on `[::1]:<allowed port>` would be reached.
- **Headscale.** The control-port allow exposes Headscale's whole HTTP surface on that port, not
  just node registration. Auth on its other routes is not verified.
- **WireGuard.** The WireGuard rules name the host's LAN or public IPv4s
  (`crates/colonizer/src/mesh.rs:424-450`), not the gateway, so they open no loopback port.

## Open risks

1. **`host` reaches every host-loopback port — closed for colonies.** The `host` profile opens
   every host-loopback port at the network level. The harness used to hand it to effectively every
   colony (mesh is on by default, and any configured provider added it), which let the untrusted
   agent drive the cockpit control API and any other loopback service
   ([#375](https://github.com/Colonizer-dev/harness/issues/375)). The harness now passes `public`
   plus port-scoped `allow@host:tcp:<control>` / `allow@host:tcp:<gateway>` rules instead, so only
   those two ports are reachable and the default deny closes the rest. The profile itself is
   unchanged upstream; the risk applies to any caller that still passes `host`. The harness's
   security review is in [audit.md](audit.md).
2. **Classifier gaps — closed for colonies by #303, (inferred) upstream.** These fall through to
   Public under `public` ([`destination.rs:40-130`][classify-all]): IPv4 `255.255.255.255` and the
   rest of `240/4`, `0.0.0.0/8` other than `0.0.0.0`, `198.18/15`, `192.0.0/24`, and the
   documentation ranges; IPv6 NAT64 `64:ff9b::/96` and `64:ff9b:1::/48`, 6to4 `2002::/16`,
   site-local `fec0::/10`. Every one but the documentation ranges is now named in the
   [always-blocked set](#egress-policy-303), which precedes the profile rules, so a colony cannot
   reach them in either mode. What remains:
   - **Documentation ranges** (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`,
     `2001:db8::/32`) are still Public and unblocked; they are not routable on the internet, so
     the practical reach is a translator or proxy that answers them.
   - **NAT64.** microsandbox dials these as ordinary public addresses ([`poll.rs:833`][tcp-direct]).
     On a host network with a NAT64 translator, an address under one of these prefixes can embed a
     private, link-local or metadata IPv4 address. A translator must drop the well-known prefix
     `64:ff9b::/96` with a non-global IPv4 address inside ([RFC 6052 §3.1][rfc6052]). That rule
     does not bind the local-use `64:ff9b:1::/48` ([RFC 8215 §5][rfc8215]). Untested. The deny
     rules now drop the prefixes themselves; whether a host's translator honours RFC 6052 is still
     the host network's to say.
3. **The host's own addresses (inferred).** A host interface address outside the private and
   link-local ranges, IPv4 or IPv6, is Public. A colony with `public` can reach any host service
   bound to that address or to all interfaces. Untested.
4. **Rules are fixed at boot.** A WireGuard rule stays for the life of the microVM after the host's
   address changes, and a new address gets no rule. Tightening any rule needs a recreate.
5. **Ingress defaults to allow** ([`types.rs:326-330`][profile-defaults]). Ingress policy applies
   to published ports ([`publisher.rs:569`][ingress-tcp], [`publisher.rs:614`][ingress-udp]), and
   the harness adds no ingress rules. It publishes one port, on host `127.0.0.1`, only when the
   mesh is off (`crates/colonizer/src/sandbox.rs:77-79`,
   `crates/colonizer/src/boot.rs:941`). Since
   [#375](https://github.com/Colonizer-dev/harness/issues/375) other colonies no longer reach that
   port through host loopback, and, as before, direct guest-to-guest traffic is denied, because
   other sandboxes' addresses fall in `172.16/12` or `fd42:6d73:62::/48`, both Private
   ([`destination.rs:79-98`][private]), and only a sandbox's own gateway is Host
   ([`destination.rs:65-70`][host-match]) (inferred).
6. **Binary provenance.** The pinned tarballs are checked by digest, but no one has tied them to
   `fa3e439`, and a non-vendored `msb` can be picked up instead
   (`crates/colonizer/src/config.rs:52-58`).

[tag]: https://github.com/superradcompany/microsandbox/tree/fa3e43902e9bc49e1d85cc0a7298e13fe2374026
[cli-flag]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L342-L358
[cli-pools]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L377-L385
[cli-profiles]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L871-L917
[cli-order]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L920-L922
[cli-netpolicy]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L848-L967
[cli-dp]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L1342-L1344
[dp-default]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/packages/microsandbox-types/rust/lib/domain.rs#L247-L254
[dp-enforce]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L185-L186
[dp-floor]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L294-L301
[dp-ports]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L523-L583
[dp-config]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/sdk/rust/lib/config/mod.rs#L151-L161
[dp-apply]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/sdk/rust/lib/backend/local/mod.rs#L231-L260
[dp-create]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/sdk/rust/lib/backend/local/sandbox/create.rs#L104
[dp-restart]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/sdk/rust/lib/backend/local/sandbox/mod.rs#L163
[config-path]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/sdk/rust/lib/config/mod.rs#L863-L871
[msb-home]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/utils/lib/lib.rs#L169-L176
[poll-floor]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L395-L399
[grammar]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/net_rule.rs#L6-L17
[rule-host]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/net_rule.rs#L563-L574
[types-hdr]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L1-L20
[profile-enum]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L69-L78
[from-profiles]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L295-L331
[profile-groups]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L316-L323
[profile-defaults]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L326-L330
[dns-query]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L600-L626
[egress-eval]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L338-L352
[dns-eval]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L589-L626
[policy-none]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L272-L279
[egress-walk]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L448-L491
[ingress]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L545-L562
[ingress-tcp]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/ports/publisher.rs#L569
[ingress-udp]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/ports/publisher.rs#L614
[allow-egress]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L769-L771
[allow-dns]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L827-L835
[classify]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L40-L60
[classify-public]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L57-L58
[classify-all]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L40-L130
[host-match]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L65-L70
[loopback]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L72-L77
[private]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L79-L98
[link-local]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L100-L113
[metadata]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L115-L123
[multicast]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/destination.rs#L125-L130
[normalize]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/addr.rs#L14-L22
[alias]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/lib.rs#L37
[families]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L206-L229
[policy-read]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L326
[bootstrap]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L410-L432
[pools]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/network.rs#L601-L656
[backend]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/backend.rs#L1-L10
[poll-hdr]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L1-L6
[poll-arc]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L263
[poll-syn]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L360-L411
[tcp53-syn]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L362-L366
[tcp53]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L551-L576
[udp-altdns]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L751-L760
[udp-check]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L764-L772
[tcp-host]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L817-L835
[tcp-direct]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L833
[udp-host]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L837-L856
[udp53]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L1108-L1113
[tcp-recheck]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/tcp/proxy.rs#L195-L216
[icmp-hdr]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/icmp/relay.rs#L1-L8
[icmp-check]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/icmp/relay.rs#L173-L184
[hosts]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/agentd/lib/network.rs#L106-L115
[resolv]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/agentd/lib/network.rs#L568-L591
[ns-doc]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/nameserver/mod.rs#L1-L14
[ns-read]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/nameserver/mod.rs#L77-L84
[fwd-policy]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/forwarder.rs#L289-L301
[fwd-alias]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/forwarder.rs#L313-L320
[fwd-rebind]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/forwarder.rs#L347-L376
[fwd-upstream]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/forwarder.rs#L797-L828
[dns-ports]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/common/ports.rs#L72-L78
[dot-policy]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/common/transport.rs#L37-L53
[dns-action]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/forwarder.rs#L834-L846
[rebind-lists]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/common/filter.rs#L12-L53
[rebind-default]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/config/types.rs#L255-L263
[modify]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/packages/microsandbox-types/rust/lib/modify.rs#L26-L79
[changelog]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/docs/changelog/2026-07-24.mdx?plain=1#L9-L18
[changelog-last]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/docs/changelog/2026-08-28.mdx?plain=1#L8
[cli-secret]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/cli/lib/commands/common.rs#L2369-L2380
[secret-tls]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/sdk/rust/lib/sandbox/builder.rs#L870-L887
[tls-defaults]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/packages/microsandbox-types/rust/lib/domain.rs#L2464-L2479
[tls-ports]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/packages/microsandbox-types/rust/lib/domain.rs#L2591-L2593
[tls-recheck]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/tls/proxy.rs#L184-L190
[quic-block]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L743-L749
[dot-syn]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L375-L382
[dot-proxy]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/netstack/poll.rs#L579-L600
[rebind-allow]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/dns/forwarder.rs#L759-L776
[explicit-egress]: https://github.com/superradcompany/microsandbox/blob/fa3e43902e9bc49e1d85cc0a7298e13fe2374026/crates/network/lib/policy/types.rs#L371-L393
[ts-socks-server]: https://github.com/tailscale/tailscale/blob/v1.102.4/cmd/tailscaled/proxy.go#L98-L101
[ts-socks-auth]: https://github.com/tailscale/tailscale/blob/v1.102.4/net/socks5/socks5.go#L158-L172
[rfc6052]: https://www.rfc-editor.org/rfc/rfc6052#section-3.1
[rfc8215]: https://www.rfc-editor.org/rfc/rfc8215#section-5
