//! The guest's IPv4 preference (#946): why the boot script rewrites `/etc/gai.conf`, and the shell
//! block it is written as.
//!
//! A colony's microVM has no working IPv6 egress — no global address of its own beyond a link-local
//! one, and no `::/0` route — but the names a colony fetches from are dual-stack: the crates.io
//! CDN, PyPI, npm's registry and the model endpoints all answer AAAA. glibc's default RFC 6724
//! precedence table ranks the native IPv6 destination `::/0` above the IPv4-mapped
//! `::ffff:0:0/96`, so a resolver that relays the AAAA record hands cargo, curl, node and python an
//! address the VM cannot route. A colony hit 403s from the crates.io CDN and worked around it by
//! pointing cargo at a third-party mirror — a supply-chain hazard the harness must not ask an agent
//! to accept — before finding that appending `precedence ::ffff:0:0/96 100` to `/etc/gai.conf`
//! fixed it.
//!
//! The harness owns no host-side seam over guest networking: `msb` takes `--net`, `--net-rule` and
//! `--net-default-egress`, and there is no tap, route, resolver or netfilter knob beyond those
//! (docs/sandbox-network.md). The one place the harness runs as the guest's root, before the agent
//! does, is the boot script (`boot_script` in `crates/colonizer/src/boot.rs`), which is also how
//! the path policy and the kernel-interface masks are applied. So the preference is written there.
//!
//! **Why two lines.** Ranking `::ffff:0:0/96` at 100 on its own does not fix a dual-stack name: that
//! prefix only ever carries an IPv4-mapped address, so what it outranks is nothing in the answer —
//! a native AAAA still arrives under `::/0` at glibc's default rank, ahead of the mapped range, and
//! is preferred. Demoting `::/0` below the mapped range is the half that does the work; the mapped
//! line only keeps an IPv4-mapped answer from being demoted along with it. Both go in.
//!
//! The block reports what it did as one `log` event in the guest's append-only event log, but only
//! when IPv6 is unusable — with a global address and a default route, an AAAA answer is reachable
//! and the table is a preference nobody needs to hear about. `/proc` is the only input: no network
//! tools, no timeouts, nothing that can hang a boot.
//!
//! Every behaviour here is proved by running [`SCRIPT`] under `sh` against temp files: the guest
//! runs the shell, so the shell is what the tests exercise.

/// The placeholder `boot.rs` splices [`SCRIPT`] into, on a line of its own in `BOOT_SCRIPT`.
pub const MARKER: &str = "@@COLONIZER_IPV6@@";

/// The prefer-IPv4 table, exactly as the script writes it: the IPv4-mapped range raised above the
/// native one, and the native default route below it. Both halves must be present, and neither
/// depends on where it sits in the file — RFC 6724 parses them into a prefix table and matches
/// longest-first, so `::ffff:0:0/96` outranks `::/0` by being the more specific prefix, not by
/// being written first. Drop either and the preference stops working; reorder them and nothing
/// changes.
#[cfg_attr(not(test), allow(dead_code))]
pub const PREFERENCE_LINES: [&str; 2] = ["precedence ::ffff:0:0/96  100", "precedence ::/0           10"];

/// The shell block, a self-contained `sh` fragment spliced into the boot script. Safe under
/// `set -u` and deliberately **not** under `set -e`: this runs as the microVM's PID 1, so a failure
/// here costs the preference and says so on stderr, never the boot. `COLONIZER_GAI_CONF`,
/// `COLONIZER_IPV6_EVENTS`, `COLONIZER_IPV6_ADDRS` and `COLONIZER_IPV6_ROUTES` parameterise the
/// four files it reads (the way the rest of the boot script parameterises `COLONIZER_WORKSPACE` and
/// `COLONIZER_PATH_POLICY`), which is how the tests below run it as-is against a temp directory and
/// a fixture `/proc` instead of the real ones.
pub const SCRIPT: &str = r#"
# IPv4 preference in the guest (issue #946; docs/sandbox-network.md). This microVM has no IPv6
# egress, but the names a colony fetches from are dual-stack, so a relayed AAAA is an address the
# VM cannot route; glibc's default RFC 6724 table would prefer it. The fix belongs in the guest's
# resolver policy, not in a colony's cargo/npm/pip configuration: a third-party package mirror is a
# supply-chain hazard, and an agent reaching for one to work around a 403 is exactly what the
# harness must not teach it. Both lines below are needed — ranking only `::ffff:0:0/96` at 100
# leaves a *native* AAAA under `::/0` at glibc's default rank, still ahead of the mapped range.
# An operator's own `precedence` line wins: this never rewrites a gai.conf that already ranks
# address families, because we cannot know why they ranked them.
ipv6_gai="${COLONIZER_GAI_CONF:-/etc/gai.conf}"
ipv6_events="${COLONIZER_IPV6_EVENTS:-/var/lib/colonizer/events.jsonl}"
ipv6_addrs="${COLONIZER_IPV6_ADDRS:-/proc/net/if_inet6}"
ipv6_routes="${COLONIZER_IPV6_ROUTES:-/proc/net/ipv6_route}"
# Is IPv6 usable at all? From /proc only, no network tools and nothing that can block: the address
# is the first field of an `addr ifindex plen scope prefixlen dev` line in hexadecimal, so `::1` is
# 32 zeros and one, and link-local `fe80::/10` is a `fe8`..`feb` prefix (0xfe80 to 0xfebf). The
# unspecified address `::` is skipped with them: it is never a source address.
ipv6_addr=
if [ -r "$ipv6_addrs" ]; then
  while read -r ipv6_addr ipv6_rest || [ -n "${ipv6_addr:-}" ]; do
    case "$ipv6_addr" in
      00000000000000000000000000000000|00000000000000000000000000000001|fe8*|fe9*|fea*|feb*)
        ipv6_addr=
        continue
        ;;
    esac
    break
  done < "$ipv6_addrs"
fi
# And a default route: /proc/net/ipv6_route is hex fields with the destination first, and `::/0` is
# 32 zeros.
ipv6_route=
if [ -r "$ipv6_routes" ]; then
  while read -r ipv6_dest ipv6_rest || [ -n "${ipv6_dest:-}" ]; do
    case "$ipv6_dest" in
      00000000000000000000000000000000) ipv6_route=1; break ;;
    esac
  done < "$ipv6_routes"
fi
# Rank address families only where the operator has not: comments and blank lines are skipped,
# leading whitespace is dropped, and a commented-out `#precedence` line — which is how a stock
# Debian image ships its own table — does not count as a preference.
ipv6_have=
if [ -f "$ipv6_gai" ]; then
  while IFS= read -r ipv6_line || [ -n "${ipv6_line:-}" ]; do
    ipv6_trim="${ipv6_line#"${ipv6_line%%[![:space:]]*}"}"
    case "$ipv6_trim" in
      ''|'#'*) ;;
      precedence*) ipv6_have=1; break ;;
    esac
  done < "$ipv6_gai"
fi
# Append inside a subshell, so an unwritable /etc — or a gai.conf this guest may not create —
# costs the preference and not the boot. `date -u` is coreutils, the same package as the cp and ln
# this script already runs; the fixed .000Z keeps the stamp RFC 3339 without GNU-only `%3N`.
ipv6_pref=
if [ -n "$ipv6_have" ]; then
  ipv6_pref="present"
elif (
  printf '\n%s\n%s\n%s\n' \
    '# colonizer (#946): this microVM has no IPv6 egress, so a dual-stack name must resolve to IPv4.' \
    'precedence ::ffff:0:0/96  100' \
    'precedence ::/0           10' \
    >>"$ipv6_gai" 2>/dev/null
); then
  ipv6_pref="applied"
else
  echo "colonizer: could not prefer IPv4 in $ipv6_gai" >&2
fi
# One event, and only where it matters: with no global address or no default route an AAAA answer
# is a trap, and the operator should know the guest is choosing IPv4 for them. The log is
# append-only and agentd reads it from the start, so this line — written before agentd starts, on a
# fresh VM, as seq 1 — is counted, replayed to the host verbatim, and numbering continues at 2.
if [ -z "$ipv6_addr" ] || [ -z "$ipv6_route" ]; then
  # The path goes into a JSON string, so the bytes that would end that string — `"`, `\`, and a
  # newline that would split the line in two — are deleted from the copy that goes into the
  # message. Nothing else in the message is a JSON metacharacter, and the surrounding format string
  # is fixed, so what remains is a parseable line for any path. In production this is the literal
  # /etc/gai.conf and nothing is dropped; `tr` is coreutils, the same package as the cp and ln this
  # script already runs. The stderr line above keeps the real path: stderr is not JSON.
  ipv6_gai_json=$(printf '%s' "$ipv6_gai" | tr -d '\\"\n')
  case "$ipv6_pref" in
    applied) ipv6_msg="no IPv6 egress in this microVM; preferring IPv4 for dual-stack names ($ipv6_gai_json written)" ;;
    present) ipv6_msg="no IPv6 egress in this microVM; preferring IPv4 for dual-stack names ($ipv6_gai_json already ranks families)" ;;
    *) ipv6_msg="no IPv6 egress in this microVM; could not prefer IPv4 for dual-stack names ($ipv6_gai_json)" ;;
  esac
  if [ -n "$ipv6_pref" ]; then ipv6_level=info; else ipv6_level=warn; fi
  ( printf '{"type":"log","level":"%s","message":"%s","seq":1,"ts":"%s"}\n' \
      "$ipv6_level" "$ipv6_msg" "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" \
      >>"$ipv6_events" 2>/dev/null ) || true
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A Debian image's `/etc/gai.conf` as shipped: its own table is there, commented out, which is
    /// exactly the case a naive `grep precedence` would mistake for a preference already set.
    const STOCK_DEBIAN: &str = r#"# This file is used by the libc resolver to rank address families.
#
# The default policy, from RFC 3484, is what the labels below express.
#label  any  precedence       50
#label  IPv4-mapped  precedence 35
#label  IPv6  precedence       40
#label  ULAs  precedence       45
#
#precedence ::ffff:0:0/96  10
#precedence ::/0           40
"#;

    /// A guest with IPv6 that works: a global address on eth0, plus the loopback address.
    const ADDRESSES_WORKING: &str = "00000000000000000000000000000001 01 80 10 80       lo\n\
                                     20010db8000000000000000000000001 03 64 00 80     eth0\n";

    /// Its routes, destination first in hexadecimal: the first line is `::/0` by way of the gateway,
    /// the rest local and link-local.
    const ROUTES_WORKING: &str = r#"00000000000000000000000000000000 00000000000000000000000000000001 80 20010db8000000000000000000000001 0000000000000000 0000000000000000 0000000000000000 00000001 0000000a 00000003 0000000000000000     eth0
00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 0000000000000000 0000000000000000 0000000000000000 00000002 00000000 0000000000000000       lo
fe80000000000000006d73fffe000602 80 00000000000000000000000000000000 00 00000000000000000000000000000000 0000000000000000 0000000000000000 0000000000000000 00000001 00000002 00000003 0000000000000000     eth0
"#;

    /// What this microVM actually has: loopback and a link-local address, nothing a packet could
    /// leave from.
    const ADDRESSES_LINK_LOCAL: &str = "00000000000000000000000000000001 01 80 10 80       lo\n\
                                       fe800000000000007087caa061111c8f 04 40 20 80 tailscale0\n";

    /// And no `::/0`: a `::1/128` loopback route is not egress either.
    const ROUTES_NO_DEFAULT: &str = r#"00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 0000000000000000 0000000000000000 0000000000000000 00000002 00000000 0000000000000000       lo
"#;

    /// The strings only the script has, pinned here so the table and the messages it reports
    /// cannot drift away from the ones an operator reads in the colony's log.
    #[test]
    fn the_script_carries_the_preference_table_and_the_messages() {
        // Both halves of the table, whichever order they are in: one line alone is the fix that
        // does not work.
        for line in PREFERENCE_LINES {
            assert!(SCRIPT.contains(&format!("'{line}'")), "the script writes {line:?}");
        }
        for message in [
            "no IPv6 egress in this microVM; preferring IPv4 for dual-stack names ($ipv6_gai_json written)",
            "no IPv6 egress in this microVM; preferring IPv4 for dual-stack names ($ipv6_gai_json already ranks families)",
            "no IPv6 egress in this microVM; could not prefer IPv4 for dual-stack names ($ipv6_gai_json)",
        ] {
            assert!(SCRIPT.contains(message), "the script reports {message:?}");
        }
        assert!(
            SCRIPT.contains(r#"{"type":"log","level":"%s","message":"%s","seq":1,"ts":"%s"}"#)
                && SCRIPT.contains("colonizer: could not prefer IPv4 in $ipv6_gai"),
            "the event shape, and a lost preference is loud on stderr"
        );
    }

    /// One test fixture's worth of files: the two paths the block writes, and the two `/proc` reads
    /// the block makes, all inside one temp directory. The `/proc` paths are parameters rather than
    /// the kernel's, so both branches — IPv6 that works and IPv6 that does not — are driven on
    /// every host, and neither test outcome depends on whether the runner has IPv6.
    #[cfg(unix)]
    struct Guest {
        root: std::path::PathBuf,
        gai: std::path::PathBuf,
        events: std::path::PathBuf,
        addrs: std::path::PathBuf,
        routes: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl Guest {
        /// A temp directory holding fixture `/proc` files with the IPv6 state given. Named the way
        /// the boot script's own test names its own.
        fn with(addrs: &str, routes: &str) -> Self {
            let root = std::env::temp_dir().join(format!("colonizer-ipv6-{}", crate::util::short_id()));
            std::fs::create_dir_all(&root).unwrap();
            let guest = Guest {
                gai: root.join("gai.conf"),
                events: root.join("events.jsonl"),
                addrs: root.join("if_inet6"),
                routes: root.join("ipv6_route"),
                root,
            };
            std::fs::write(&guest.addrs, addrs).unwrap();
            std::fs::write(&guest.routes, routes).unwrap();
            guest
        }

        /// This microVM: link-local only, and no default route.
        fn without_ipv6_egress() -> Self {
            Guest::with(ADDRESSES_LINK_LOCAL, ROUTES_NO_DEFAULT)
        }

        /// Runs [`SCRIPT`] under `sh` the way
        /// `boot_script_links_colonizer_svc_and_says_so_when_it_cannot` runs its own slice of the
        /// boot script: `set -u` (no `set -e`), a sentinel after the block, and the script's own
        /// exit status.
        fn run(&self) -> std::process::Output {
            self.run_with_gai(&self.gai)
        }

        /// The same, with a gai.conf path of the test's choosing — one the block cannot write, or
        /// one whose name is hostile to the JSON the block writes.
        fn run_with_gai(&self, gai: &std::path::Path) -> std::process::Output {
            // The temp paths these tests build are the script's only input, and a single quote in
            // one must not become two arguments.
            let q = |path: &std::path::Path| format!("'{}'", path.display().to_string().replace('\'', r"'\''"));
            let script = format!(
                "set -u\nCOLONIZER_GAI_CONF={}\nCOLONIZER_IPV6_EVENTS={}\n\
                 COLONIZER_IPV6_ADDRS={}\nCOLONIZER_IPV6_ROUTES={}\n{}echo booted",
                q(gai),
                q(&self.events),
                q(&self.addrs),
                q(&self.routes),
                SCRIPT,
            );
            std::process::Command::new("sh").arg("-c").arg(script).output().unwrap()
        }

        /// The one event the block wrote, parsed the way agentd parses it.
        fn event(&self) -> serde_json::Value {
            let line = std::fs::read_to_string(&self.events)
                .unwrap_or_else(|e| panic!("the block wrote an event ({e}), and its text is {}", self.events.display()));
            assert_eq!(line.lines().count(), 1, "exactly one event: {line:?}");
            serde_json::from_str(line.trim()).expect("agentd parses this line back")
        }

        /// The guest booted: exit 0, the sentinel printed, nothing on stderr.
        fn assert_booted_quietly(&self, out: &std::process::Output) {
            assert!(out.status.success(), "a boot that fails is worse than a missing preference");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "booted", "boot carries on");
            assert!(
                out.stderr.is_empty(),
                "a preference that lands is silent: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        fn finish(&self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    /// A fresh gai.conf gets the whole table, once, and a second run leaves it alone. The guest
    /// rootfs is discardable, so every boot is a fresh `/etc` and the table is written afresh —
    /// what must hold across boots is the operator's table surviving, not our lines accumulating.
    #[cfg(unix)]
    #[test]
    fn the_table_is_appended_once_and_never_duplicated() {
        let guest = Guest::without_ipv6_egress();
        std::fs::write(&guest.gai, STOCK_DEBIAN).unwrap();

        guest.assert_booted_quietly(&guest.run());
        let after_first = std::fs::read_to_string(&guest.gai).unwrap();
        assert!(
            after_first.starts_with(STOCK_DEBIAN),
            "the operator's file is appended to, not rewritten"
        );
        for line in PREFERENCE_LINES {
            assert!(after_first.contains(line), "{line:?} was written");
        }
        assert_eq!(
            after_first.lines().filter(|l| l.trim() == PREFERENCE_LINES[0]).count(),
            1,
            "one active line, and the stock image's own commented one untouched: {after_first}"
        );

        guest.assert_booted_quietly(&guest.run());
        assert_eq!(
            std::fs::read_to_string(&guest.gai).unwrap(),
            after_first,
            "a second boot changes nothing"
        );
        guest.finish();
    }

    /// A commented-out table is not a preference, so it does not stop ours: the stock Debian image
    /// ships exactly this, and the shell reads the same lines a naive `grep precedence` would not.
    #[cfg(unix)]
    #[test]
    fn a_commented_out_table_does_not_count_as_one() {
        let guest = Guest::without_ipv6_egress();
        // Our own lines and a stock label, all commented out: text that matches, ranking nothing.
        let operator = format!(
            "#precedence ::/0           10\n#label any precedence 50\n#{}",
            PREFERENCE_LINES[0]
        );
        std::fs::write(&guest.gai, &operator).unwrap();

        guest.assert_booted_quietly(&guest.run());
        let written = std::fs::read_to_string(&guest.gai).unwrap();
        for line in PREFERENCE_LINES {
            assert_eq!(
                written.lines().filter(|l| l.trim() == line).count(),
                1,
                "{line:?} was written once, and the commented copy is still just a comment: {written}"
            );
        }
        assert!(written.starts_with(&operator), "and nothing above the append was rewritten");
        guest.finish();
    }

    /// The two ends of the one switch: a gai.conf that already ranks families is the operator's,
    /// not ours, and is left byte for byte alone; a guest with no `/etc/gai.conf` at all is the
    /// other end, and the block creates one and still boots.
    #[cfg(unix)]
    #[test]
    fn an_operator_table_is_left_alone_and_a_missing_one_is_created() {
        let guest = Guest::without_ipv6_egress();
        let operator = "# our ordering\nprecedence ::/0 40\nprecedence ::ffff:0:0/96 100\n";
        std::fs::write(&guest.gai, operator).unwrap();

        guest.assert_booted_quietly(&guest.run());
        assert_eq!(
            std::fs::read_to_string(&guest.gai).unwrap(),
            operator,
            "the operator's file is not rewritten"
        );
        std::fs::remove_file(&guest.gai).unwrap();

        guest.assert_booted_quietly(&guest.run());
        let written = std::fs::read_to_string(&guest.gai).expect("the file is created");
        for line in PREFERENCE_LINES {
            assert!(written.contains(line), "{line:?} was written to a fresh file");
        }
        guest.finish();
    }

    /// The event, in the state that earns one: no global address and no default route, so an AAAA
    /// answer is a trap. One line, with the fields agentd needs to replay it, and at the level that
    /// follows whether the preference landed.
    #[cfg(unix)]
    #[test]
    fn the_event_is_one_well_formed_line_when_ipv6_is_unusable() {
        let guest = Guest::without_ipv6_egress();
        std::fs::write(&guest.gai, STOCK_DEBIAN).unwrap();

        guest.assert_booted_quietly(&guest.run());
        let event = guest.event();
        assert_eq!(event["type"], "log");
        assert_eq!(event["level"], "info", "the preference landed: {event}");
        assert_eq!(event["seq"], 1, "seq 1 is what agentd counts from: {event}");
        let ts = event["ts"].as_str().expect("a timestamp");
        chrono::DateTime::parse_from_rfc3339(ts).expect("agentd's own stamp format");
        let message = event["message"].as_str().expect("a message");
        assert!(message.contains("preferring IPv4"), "{message}");
        assert!(message.contains(&guest.gai.display().to_string()), "{message}");
        guest.finish();
    }

    /// The other half of that condition, on its own: with a global address and a default route the
    /// table is a preference nobody needs to hear about, so nothing is written and nothing is said
    /// — no event file, no stderr, and the same gai.conf either way.
    #[cfg(unix)]
    #[test]
    fn a_guest_with_working_ipv6_gets_no_event_and_no_noise() {
        let guest = Guest::with(ADDRESSES_WORKING, ROUTES_WORKING);
        std::fs::write(&guest.gai, STOCK_DEBIAN).unwrap();

        guest.assert_booted_quietly(&guest.run());
        assert!(
            !guest.events.exists(),
            "with working IPv6 there is nothing to report: {}",
            guest.events.display()
        );
        let written = std::fs::read_to_string(&guest.gai).unwrap();
        for line in PREFERENCE_LINES {
            assert!(written.contains(line), "{line:?} was written regardless");
        }
        guest.finish();
    }

    /// A gai.conf the guest cannot write costs the preference, not the boot: the failure says so
    /// in the script's own voice, and so does the event.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_gai_conf_is_loud_and_never_fails_the_boot() {
        let guest = Guest::without_ipv6_egress();
        // A directory that is not there: the append cannot create the file, as root or not.
        let gai = guest.root.join("absent").join("gai.conf");

        let out = guest.run_with_gai(&gai);
        assert!(out.status.success(), "the boot carries on without the preference");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "booted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("colonizer: could not prefer IPv4 in {}", gai.display())),
            "{stderr}"
        );
        assert!(!gai.exists(), "nothing was written");

        let event = guest.event();
        assert_eq!(event["level"], "warn", "a preference that did not land is a warning: {event}");
        assert_eq!(event["seq"], 1);
        assert_eq!(event["type"], "log");
        guest.finish();
    }

    /// The message carries the gai.conf path into a JSON string, and the tests above only ever put
    /// a temp path in it. A path carrying the bytes that end a JSON string — `"`, `\` — or a
    /// newline that splits the line in two must still leave agentd a line it can parse: a broken
    /// event is dropped silently, which is the one failure this block cannot afford.
    #[cfg(unix)]
    #[test]
    fn a_path_with_json_hostile_bytes_still_leaves_a_line_agentd_can_parse() {
        let guest = Guest::without_ipv6_egress();
        let hostile = guest.root.join("qu\"ote\\d").join("gai\nname");
        std::fs::create_dir_all(hostile.parent().unwrap()).unwrap();

        let out = guest.run_with_gai(&hostile);
        assert!(out.status.success(), "the boot carries on");

        let event = guest.event();
        let message = event["message"].as_str().expect("a message");
        // The bytes that would have broken the string are gone, and what is left is the path an
        // operator would recognise.
        let expected = hostile.display().to_string().replace(['"', '\\', '\n'], "");
        assert!(message.contains(&expected), "{message:?} should carry {expected:?}");
        guest.finish();
    }
}
