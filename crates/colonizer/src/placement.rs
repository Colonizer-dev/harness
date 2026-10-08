//! Where a colony would run (issue #688): a pure policy over this member and the fleet's last-known
//! peers. Cross-member execution does not exist yet (issue #1252), so nothing here runs a colony
//! anywhere but locally: it decides and says why, and `sessions::launch` records that reason on the
//! colony it starts. No state, no I/O — the caller builds the candidates.

use crate::fleet::{HostHealth, HostSummary};

/// One fleet member as placement sees it: its identity, whether it is this machine, whether it
/// answers, and what it offers — platform, KVM verdict, free microVM slots. From a [`HostSummary`].
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    /// This is the member the launch runs on: the mothership making the decision.
    pub local: bool,
    pub online: bool,
    pub platform: String,
    /// `None` where the platform has no `/dev/kvm` to check (a Mac), or a peer too old to report it.
    pub kvm: Option<bool>,
    pub slots_free: u32,
}

impl Candidate {
    /// A member as placement sees it: the free slot count is the ceiling less what is in use, never negative.
    pub fn from_summary(summary: &HostSummary, local: bool) -> Candidate {
        Candidate {
            id: summary.id.clone(),
            name: summary.name.clone(),
            local,
            online: summary.health == HostHealth::Online,
            platform: summary.platform.clone(),
            kvm: summary.kvm,
            slots_free: summary
                .slots_ceiling
                .saturating_sub(summary.slots_in_use)
                .min(u32::MAX as usize) as u32,
        }
    }
}

/// Where a colony would go: the member [`place`] chose, and the sentence that says why. `local` is
/// always true today, since cross-member launches are not built (issue #1252).
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub host_name: String,
    pub local: bool,
    pub reason: String,
}

/// Why placement could not honour a pin. Unpinned placement never refuses: it falls back to this
/// member, where the queue holds the colony exactly as today.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    UnknownHost { pin: String },
    Ineligible { host: String, reason: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::UnknownHost { pin } => write!(f, "no fleet member matches \"{pin}\""),
            Refusal::Ineligible { host, reason } => write!(f, "cannot place the colony on {host}: {reason}"),
        }
    }
}

/// Places a colony: honour `pin` when given (by exact id, or name case-insensitively), else prefer
/// `local` and fall back to the roomiest peer. Only a pin can refuse.
pub fn place(pin: Option<&str>, local: &Candidate, peers: &[Candidate]) -> Result<Placement, Refusal> {
    match pin.map(str::trim).filter(|pin| !pin.is_empty()) {
        Some(pin) => place_pinned(pin, local, peers),
        None => Ok(place_free(local, peers)),
    }
}

/// A pinned launch never falls back elsewhere, and pinning to this member is today's behaviour
/// whatever its capacity: an operator who named one machine did not ask to have the colony moved.
fn place_pinned(pin: &str, local: &Candidate, peers: &[Candidate]) -> Result<Placement, Refusal> {
    let candidate = std::iter::once(local)
        .chain(peers)
        .find(|c| c.id == pin || c.name.eq_ignore_ascii_case(pin))
        .ok_or_else(|| Refusal::UnknownHost { pin: pin.to_string() })?;
    if !candidate.local
        && let Some(reason) = ineligibility(candidate)
    {
        return Err(Refusal::Ineligible {
            host: candidate.name.clone(),
            reason: reason.to_string(),
        });
    }
    Ok(placement_for(candidate, format!("pinned to {}", candidate.name)))
}

/// An unpinned launch: this member when it can take the colony, else the roomiest peer, else back
/// here to queue. This member is always a candidate, so this never fails.
fn place_free(local: &Candidate, peers: &[Candidate]) -> Placement {
    if let Some(why) = ineligibility(local) {
        if let Some(peer) = best_remote(peers) {
            let reason = format!("{}: {why}; {} has room ({})", local.name, peer.name, slots(peer));
            return placement_for(peer, reason);
        }
        let reason = format!("{}: {why}; no member has room", local.name);
        return placement_for(local, reason);
    }
    placement_for(local, format!("{}: {}", local.name, slots(local)))
}

/// The eligible peer with the most free slots, ties broken by name; `None` when no peer qualifies.
fn best_remote(peers: &[Candidate]) -> Option<&Candidate> {
    peers
        .iter()
        .filter(|c| ineligibility(c).is_none())
        .min_by(|a, b| b.slots_free.cmp(&a.slots_free).then_with(|| a.name.cmp(&b.name)))
}

/// Why a member cannot take a colony, or `None` when it can. Linux needs a working `/dev/kvm`; a peer
/// too old to report one reads `None` and is "KVM unknown" rather than assumed broken, and a Mac needs
/// Apple Silicon (`darwin-arm64`), so everything else — an Intel Mac, `other` — is unsupported.
fn ineligibility(candidate: &Candidate) -> Option<&'static str> {
    if !candidate.online {
        return Some("host unreachable");
    }
    if candidate.platform.starts_with("linux") {
        match candidate.kvm {
            Some(true) => {}
            Some(false) => return Some("KVM unavailable"),
            None => return Some("KVM unknown"),
        }
    } else if !candidate.platform.contains("arm64") && !candidate.platform.contains("aarch64") {
        return Some("unsupported platform");
    }
    (candidate.slots_free == 0).then_some("no free slot")
}

/// The free capacity in words: "3 free slots" / "1 free slot".
fn slots(candidate: &Candidate) -> String {
    let n = candidate.slots_free;
    format!("{n} free slot{}", if n == 1 { "" } else { "s" })
}

fn placement_for(candidate: &Candidate, reason: String) -> Placement {
    Placement {
        host_name: candidate.name.clone(),
        local: candidate.local,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "linux-x86_64";
    const MAC_ARM: &str = "darwin-arm64";

    fn cand(id: &str, name: &str, local: bool, online: bool, platform: &str, kvm: Option<bool>, free: u32) -> Candidate {
        Candidate {
            id: id.into(),
            name: name.into(),
            local,
            online,
            platform: platform.into(),
            kvm,
            slots_free: free,
        }
    }
    fn local(free: u32) -> Candidate {
        cand("host-local", "this-box", true, true, LINUX, Some(true), free)
    }
    fn peer(id: &str, name: &str, free: u32) -> Candidate {
        cand(id, name, false, true, LINUX, Some(true), free)
    }

    #[test]
    fn an_eligible_local_member_wins_else_the_roomiest_eligible_peer_else_here() {
        // This member when eligible, whatever a roomier peer offers.
        let chosen = place(None, &local(1), &[peer("p1", "peer-one", 9)]).unwrap();
        assert!(chosen.local && chosen.reason == "this-box: 1 free slot");
        // Full here: the roomiest eligible peer, ties broken by name.
        let peers = [peer("p1", "peer-one", 1), peer("p2", "peer-two", 3)];
        let chosen = place(None, &local(0), &peers).unwrap();
        let want = "this-box: no free slot; peer-two has room (3 free slots)";
        assert_eq!((chosen.host_name.as_str(), chosen.reason.as_str()), ("peer-two", want));
        let tied = [peer("p1", "zeta", 2), peer("p2", "alpha", 2)];
        assert_eq!(place(None, &local(0), &tied).unwrap().host_name, "alpha");
        // Nobody eligible: back here to queue, as today.
        let full = [peer("p1", "peer-one", 0)];
        assert!(place(None, &local(0), &full).unwrap().local);
    }

    #[test]
    fn only_a_member_that_can_boot_a_microvm_is_eligible() {
        for (platform, kvm, want) in [
            (LINUX, Some(true), None),
            (LINUX, Some(false), Some("KVM unavailable")),
            (LINUX, None, Some("KVM unknown")),
            (MAC_ARM, None, None),
            ("other", None, Some("unsupported platform")),
        ] {
            let c = cand("p1", "peer", false, true, platform, kvm, 1);
            assert_eq!(ineligibility(&c), want, "{platform}");
        }
        // The Mac that can boot wins over the KVM-less member here and the roomy, unknown peer.
        let peers = [
            cand("p1", "intel-mac", false, true, "other", None, 2),
            cand("p2", "silicon-mac", false, true, MAC_ARM, None, 1),
            cand("p3", "old-peer", false, true, LINUX, None, 5),
        ];
        let here = cand("host-local", "this-box", true, true, LINUX, Some(false), 4);
        let chosen = place(None, &here, &peers).unwrap();
        let want = "this-box: KVM unavailable; silicon-mac has room (1 free slot)";
        assert_eq!((chosen.host_name.as_str(), chosen.reason.as_str()), ("silicon-mac", want));
        // An unreachable member is never chosen, whatever it offers.
        let down = cand("p1", "down-box", false, false, LINUX, Some(true), 4);
        assert_eq!(ineligibility(&down), Some("host unreachable"));
        assert!(place(None, &local(0), &[down]).unwrap().local);
    }

    #[test]
    fn a_pin_picks_its_member_and_never_falls_back() {
        let peers = [peer("p1", "Peer-One", 2)];
        for pin in ["p1", "peer-one", "PEER-ONE"] {
            let chosen = place(Some(pin), &local(0), &peers).unwrap();
            assert_eq!(chosen.host_name, "Peer-One", "{pin}");
            assert!(!chosen.local && chosen.reason == "pinned to Peer-One", "{pin}");
        }
        // Pinning to this member is today's behaviour whatever its capacity.
        assert!(place(Some("this-box"), &local(0), &peers).unwrap().local);
        // A pinned member that cannot take the colony is refused, never moved here.
        let down = cand("p1", "down-box", false, false, LINUX, Some(true), 5);
        let no_kvm = cand("p2", "no-kvm", false, true, LINUX, Some(false), 5);
        let refused = place(Some("down-box"), &local(9), &[down]).unwrap_err();
        assert_eq!(refused.to_string(), "cannot place the colony on down-box: host unreachable");
        let refused = place(Some("p2"), &local(9), &[no_kvm]).unwrap_err();
        assert_eq!(refused.to_string(), "cannot place the colony on no-kvm: KVM unavailable");
        // An unknown pin is refused; a blank one is no pin at all.
        let refusal = place(Some("nope"), &local(9), &[]).unwrap_err();
        assert_eq!(refusal, Refusal::UnknownHost { pin: "nope".into() });
        assert_eq!(refusal.to_string(), "no fleet member matches \"nope\"");
        assert!(place(Some("  "), &local(9), &[]).unwrap().local);
    }

    #[test]
    fn a_candidate_reads_its_summary() {
        let mut summary = HostSummary {
            kvm: Some(true),
            slots_in_use: 3,
            slots_ceiling: 4,
            health: HostHealth::Unreachable,
            ..Default::default()
        };
        let candidate = Candidate::from_summary(&summary, true);
        assert!(candidate.local && !candidate.online && candidate.slots_free == 1);
        // A ceiling below the live count never underflows into free slots.
        summary.slots_in_use = 9;
        assert_eq!(Candidate::from_summary(&summary, false).slots_free, 0);
    }
}
