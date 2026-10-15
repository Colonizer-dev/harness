//! `colonizer doctor`: what the host is missing, and the check that settles it.
//!
//! A bare `OS error 38 (Function not implemented)` out of mesh or sandbox setup says that
//! something the process asked for does not exist, and nothing else — which component, which
//! kernel feature, what to run. This module is the one place that maps a failure to the health
//! check that explains it plus the next command, so the failure sites ([`crate::mesh`],
//! [`crate::sandbox`]) and `colonizer doctor` itself cannot drift apart into two different
//! explanations of the same errno.
//!
//! Read-only throughout: nothing here starts a daemon, writes a state directory, or boots a
//! microVM. It answers "can this host run colonies", and never "are colonies running".

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{config::Settings, mesh, runtime, version};

/// Which part of the setup failed. The same errno means different things to different components,
/// so the sentence names the one that failed rather than the machine it failed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Component {
    /// The headscale control plane this repo vendors and supervises.
    MeshControl,
    /// The harness's own userspace `tailscaled` node.
    MeshNode,
    /// The microVM a colony runs in.
    MicroVm,
}

impl Component {
    /// How the component names itself in a sentence meant for a person.
    fn label(&self) -> &'static str {
        match self {
            Component::MeshControl => "headscale",
            Component::MeshNode => "tailscaled",
            Component::MicroVm => "the microVM sandbox",
        }
    }
}

/// A failure, the health check that explains it, and nothing else: no fix is attempted here, only
/// named, so a diagnosis never guesses past what the check can settle.
pub(crate) struct Diagnosis {
    pub(crate) component: &'static str,
    pub(crate) cause: &'static str,
    pub(crate) check: &'static str,
}

impl fmt::Display for Diagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} could not be set up: {}. The check that settles it: {}. Next: `colonizer doctor`.",
            self.component, self.cause, self.check
        )
    }
}

/// The errnos this table explains, and nothing else: an errno with no known cause leaves the
/// caller's own error exactly as it was, because a wrong explanation is worse than none.
pub(crate) fn diagnose(component: Component, errno: i32) -> Option<Diagnosis> {
    let (cause, check) = match (component, errno) {
        (Component::MicroVm, libc::ENOSYS) => (
            "the guest kernel does not implement a syscall the sandbox runtime needs",
            "`msb version` names the runtime; libkrunfw is built without landlock, and /dev/kvm has to be readable and writable",
        ),
        (Component::MeshControl | Component::MeshNode, libc::ENOSYS) => (
            "the kernel does not implement a syscall the mesh daemon needs",
            "the kernel has KVM (CONFIG_KVM with kvm_intel or kvm_amd loaded) and /dev/kvm exists",
        ),
        (_, libc::ENODEV) => (
            "there is no such device: /dev/kvm is absent",
            "ls -l /dev/kvm after loading the kvm_intel or kvm_amd module",
        ),
        (_, libc::EACCES | libc::EPERM) => (
            "permission to /dev/kvm was refused",
            "ls -l /dev/kvm, and the kvm group has to be readable and writable by this user",
        ),
        (Component::MicroVm, libc::ENOENT) => (
            "the msb microVM binary is not on disk",
            "COLONIZER_MSB has to name the msb binary this install shipped",
        ),
        (Component::MeshControl | Component::MeshNode, libc::ENOENT) => (
            "the vendored mesh binary is not on disk",
            "scripts/install.sh vendors headscale and tailscale into the app's vendor/ directory",
        ),
        _ => return None,
    };
    Some(Diagnosis {
        component: component.label(),
        cause,
        check,
    })
}

/// The same table, reached the way a child's failure reaches it. `util::exec` folds a failed
/// child's stderr into the error string (`bail!("\`{desc}\` failed ({}): {}", out.status, stderr)`),
/// so a wait or a join failure carries its errno as text rather than as an `io::Error`. Read as
/// data — one row per needle, so adding one is a line and not a branch.
const ERRNO_TEXT: &[(&str, i32)] = &[
    ("enosys", libc::ENOSYS),
    ("function not implemented", libc::ENOSYS),
    ("no such device", libc::ENODEV),
    ("permission denied", libc::EACCES),
    ("operation not permitted", libc::EPERM),
    ("no such file or directory", libc::ENOENT),
];

pub(crate) fn diagnose_text(component: Component, text: &str) -> Option<Diagnosis> {
    let text = text.to_lowercase();
    ERRNO_TEXT
        .iter()
        .find(|(needle, _)| text.contains(needle))
        .and_then(|(_, errno)| diagnose(component, *errno))
}

/// What an error says about itself, through either door: an `io::Error`'s errno when it has one,
/// its own text otherwise.
pub(crate) fn explain<E: std::error::Error + 'static>(component: Component, err: &E) -> Option<Diagnosis> {
    // The `'static` bound is `downcast_ref`'s own: it compares a `TypeId`, so the trait object has
    // to be one. Every error this is called with is `'static` already.
    let as_std: &(dyn std::error::Error + 'static) = err;
    let errno = as_std.downcast_ref::<io::Error>().and_then(io::Error::raw_os_error);
    // No errno is not a dead end, only the first door being shut: `and_then` rather than `?`, or an
    // error with no raw errno would never reach the text.
    errno
        .and_then(|errno| diagnose(component, errno))
        .or_else(|| diagnose_text(component, &err.to_string()))
}

/// The error a setup step reports: what it was doing, what went wrong, and — when the errno is
/// one the table explains — the check that settles it and the command to run. The source chain is
/// deliberately dropped: the original text is already in the message, and an operator reading a
/// boot failure needs the check, not another frame to unwrap.
pub(crate) fn failed<E: std::error::Error + 'static>(component: Component, what: &str, err: &E) -> anyhow::Error {
    compose(what, &err.to_string(), explain(component, err))
}

/// [`failed`] for an [`anyhow::Error`], which deliberately does not implement
/// [`std::error::Error`] and so has no errno left to downcast to — its text is all there is.
///
/// Every `wait_for` and every `exec` failure arrives as one of these, folded by `util::exec` out
/// of a child's stderr, so this is the form the issue's ENOSYS mostly takes.
pub(crate) fn failed_anyhow(component: Component, what: &str, err: &anyhow::Error) -> anyhow::Error {
    let text = err.to_string();
    compose(what, &text, diagnose_text(component, &text))
}

fn compose(what: &str, text: &str, diagnosis: Option<Diagnosis>) -> anyhow::Error {
    let mut message = format!("{what}: {text}");
    if let Some(diagnosis) = diagnosis {
        message.push('\n');
        message.push_str(&diagnosis.to_string());
    }
    anyhow::Error::msg(message)
}

/// The Landlock ABI version this kernel implements — the number the guest kernel's LSM list is
/// measured against — or the errno that says it implements none.
///
/// `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` is the documented way to
/// ask: a null `ruleset_attr` with flag 1 creates no ruleset and returns the version instead. The
/// error is built with `from_raw_os_error` so `raw_os_error()` is the real ENOSYS, which is what
/// makes [`diagnose`] explain it.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn landlock() -> Result<u32> {
    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    // A null `ruleset_attr` with a zero size: the flags word is the only thing telling the kernel
    // this is a version query rather than a ruleset being created.
    const NO_RULESET: usize = 0;
    const NO_RULES: usize = 0;
    let answer = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            NO_RULESET,
            NO_RULES,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if answer < 0 {
        // The syscall answers with the negated errno.
        return Err(io::Error::from_raw_os_error(-answer as i32).into());
    }
    Ok(answer as u32)
}

/// Landlock is a Linux LSM, so there is nothing to ask for anywhere else, and the signature is kept
/// so the caller does not need a `cfg` of its own.
#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
pub(crate) fn landlock() -> Result<u32> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "landlock is a Linux feature").into())
}

/// The three vendored mesh binaries, relative to the assets directory. The same three
/// [`mesh::binaries_present`] looks for, spelled out so a missing one can be named: it answers
/// yes/no, and "which one" is what an operator needs.
const MESH_BINARIES: &[(&str, &str)] = &[
    ("vendor/headscale", "headscale"),
    ("vendor/tailscale/tailscale", "tailscale"),
    ("vendor/tailscale/tailscaled", "tailscaled"),
];

/// `colonizer doctor` — what this host can run colonies on, and what to do about the rest.
///
/// A report, not a gate: it exits 0 even with failing checks, so it can be run in a loop and read
/// as text, and the closing line says plainly whether anything is wrong.
pub(crate) async fn command() -> Result<()> {
    let cfg = Settings::from_env()?;
    println!("colonizer {}", version::build().line());
    println!("read-only: this starts nothing and writes nothing");
    println!();
    let mut failing = 0;

    report(
        &mut failing,
        "assets",
        match &cfg.assets {
            Some(assets) if assets.is_dir() => Ok(assets.display().to_string()),
            // Not an app directory is the same root cause as no setting at all, so it is the same
            // sentence with the path the setting named in front of it.
            assets => Err(assets_missing(assets.as_ref().map(PathBuf::as_path))),
        },
    );

    report(&mut failing, "kernel", Ok(kernel_line()));
    // The probe asks this host, not a guest, so its errno is read by `host_landlock_failure`: the
    // microVM row would answer about libkrunfw's kernel, and its permission row about /dev/kvm, and
    // an ENOSYS here is a fact about the kernel the operator is running on. Anything that errno is
    // not falls through to the same composed error a boot failure prints.
    report(
        &mut failing,
        "landlock",
        landlock()
            .map(|v| format!("(ABI version {v})"))
            .map_err(|e| match host_landlock_failure(&e) {
                Some(line) => line,
                None => format!("{:#}", failed_anyhow(Component::MicroVm, LANDLOCK_SYSCALL, &e)),
            }),
    );

    report(
        &mut failing,
        "kvm",
        match runtime::probe_kvm().await {
            // No KVM off Linux: nothing is missing, since colonies are KVM microVMs elsewhere.
            None => Ok("(not required on this platform)".to_string()),
            Some(kvm) if kvm.ok => Ok(String::new()),
            Some(kvm) => {
                let error = kvm.error.unwrap_or_default();
                // WSL answers to Windows, not to the table: on WSL2 an absent `/dev/kvm` is
                // nested virtualization, on WSL1 there is no kvm at all — neither is a module
                // nobody loaded.
                Err(wsl_kvm_failure(on_wsl(), Path::new("/dev/kvm").exists()).unwrap_or_else(|| kvm_failure(&error)))
            }
        },
    );

    match &cfg.assets {
        // Absent mesh binaries are not a fault: an install that never vendored them runs colonies
        // on a loopback port (the same reasoning as `status::mesh_status`, which keeps this out of
        // its `error` field so it does not paint a healthy Mac red). The check is reported, named
        // and not counted.
        Some(assets) if !mesh::binaries_present(assets) => {
            let missing: Vec<&str> = MESH_BINARIES
                .iter()
                .filter(|(rel, _)| !assets.join(rel).exists())
                .map(|(_, name)| *name)
                .collect();
            let check = diagnose(Component::MeshControl, libc::ENOENT)
                .map(|d| d.check)
                .unwrap_or_default();
            println!(
                "{:<13}{} are not vendored, so mesh is not set up on this host and colonies use a loopback port: {check}",
                "mesh binaries",
                missing.join(" and ")
            );
        }
        Some(_) => report(
            &mut failing,
            "mesh binaries",
            Ok("headscale, tailscale and tailscaled".to_string()),
        ),
        None => {}
    }

    report(
        &mut failing,
        "microvm",
        match resolve_msb(&cfg.msb) {
            Some(path) => Ok(path.display().to_string()),
            None => Err(format!("{}: {}", cfg.msb, why(Component::MicroVm, libc::ENOENT))),
        },
    );

    println!();
    println!(
        "{}",
        if failing == 0 {
            "all checks passed".to_string()
        } else {
            format!("{failing} check(s) failing, each named above with the check that settles it")
        }
    );
    Ok(())
}

/// One check's line, and the tally behind the closing line: `ok …` when it passed, otherwise
/// whatever explains it. A failing check counts itself, so nothing has to be counted twice.
fn report(failing: &mut usize, check: &str, outcome: std::result::Result<String, String>) {
    match outcome {
        Ok(ok) if ok.is_empty() => println!("{check:<13}ok"),
        Ok(ok) => println!("{check:<13}ok {ok}"),
        Err(why) => {
            *failing += 1;
            println!("{check:<13}{why}");
        }
    }
}

/// A cause and its check, without the component name or the `Next:` line: the line already names
/// which check failed, and inside `doctor` the last of those would point at the command already
/// running. A [`Diagnosis`] rendered the way every other line in this module renders one.
fn bare(cause: &str, check: &str) -> String {
    format!("{cause}. {check}")
}

/// [`bare`] for an errno the table explains, and nothing at all for one it does not.
fn why(component: Component, errno: i32) -> String {
    diagnose(component, errno).map(|d| bare(d.cause, d.check)).unwrap_or_default()
}

/// The syscall [`landlock`] makes, spelled once so the lines that name it cannot drift apart.
const LANDLOCK_SYSCALL: &str = "landlock_create_ruleset";

/// What [`landlock`]'s errno means when the probe that hit it was *this host*'s.
///
/// [`landlock`] asks the kernel the operator is running on, so its errnos are not the microVM's: a
/// guest ENOSYS is about libkrunfw's kernel and a guest EPERM is about `/dev/kvm`, and neither is
/// what happened here. The ENOSYS is the one that stays — it is a fact about the host kernel, and
/// the same fact the ENOSYS rows above state for the components they belong to, pointed at this
/// kernel. A refusal is a fact about the policy around the probe instead, which is why this does
/// not borrow the `/dev/kvm` sentence: telling an operator their KVM permissions are wrong because
/// seccomp would not let `colonizer` make a syscall is how a correct-looking line teaches the wrong
/// thing.
///
/// `None` for an errno that is neither, so the caller can fall through to the same `failed_anyhow`
/// a boot failure would have printed rather than swallow the error.
fn host_landlock_failure(err: &anyhow::Error) -> Option<String> {
    let errno = err.downcast_ref::<io::Error>().and_then(io::Error::raw_os_error)?;
    let (cause, check) = match errno {
        libc::ENOSYS => (
            format!(
                "the host kernel does not implement {LANDLOCK_SYSCALL}, so there is no Landlock LSM here for a colony to be confined against"
            ),
            "the kernel was built with CONFIG_SECURITY_LANDLOCK, and `cat /proc/cmdline` does not carry landlock_disabled=1",
        ),
        libc::EACCES | libc::EPERM => (
            format!(
                "the {LANDLOCK_SYSCALL} probe was refused before it ran, by a seccomp policy or a restrictive container around this process. That says nothing about whether colonies can boot here: it is the policy around `colonizer` refusing the probe, not the kernel, so this line is only meaningful from somewhere that policy lets the syscall through"
            ),
            "the seccomp profile around this process allows landlock_create_ruleset",
        ),
        _ => return None,
    };
    Some(bare(&cause, check))
}

/// The way out of a missing assets directory, in the words [`crate::app`] uses when it refuses to
/// build a mesh manager without one (`app assets not found: run scripts/install.sh`). One constant,
/// so the daemon that will not start and `doctor` cannot name two different commands.
const ASSETS_CHECK: &str = "The way out: run scripts/install.sh, which builds the app directory — the vendored mesh binaries and the rest of the assets — that this build reads them from";

/// Why the assets check failed, in a sentence of its own. Deliberately not a [`Diagnosis`]: a
/// missing assets directory is the root cause of every missing vendored file, so routing it through
/// a component would tell an operator a mesh binary is missing when the directory holding all
/// three of them is not there at all.
fn assets_missing(path: Option<&Path>) -> String {
    let named = match path {
        Some(path) => format!("{} is not a directory", path.display()),
        None => "the app assets directory could not be found".to_string(),
    };
    format!("{named}, so nothing this install ships is on disk. {ASSETS_CHECK}")
}

/// A KVM the probe could not open, in the words the operator would have seen on the boot that hit
/// the same wall: the probe knows only that it failed, so the errno is derived from the permission
/// it is, not parsed back out of the prose.
fn kvm_failure(error: &str) -> String {
    let diagnosis = diagnose_text(Component::MicroVm, error)
        .or_else(|| diagnose(Component::MicroVm, libc::EACCES))
        .expect("both permissions have an entry");
    format!("{error}. {}", diagnosis.check)
}

/// Which WSL a kernel release says it is. The distinction decides the kvm row's words: WSL2 is a
/// VM that can pass virtualization through, WSL1 is a syscall translation layer, not a VM, and
/// has no `/dev/kvm` on any setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wsl {
    /// The WSL1 kernel: a translation layer, so no microVM can run here at all.
    One,
    /// The WSL2 kernel: a real VM, and nested virtualization is the knob.
    Two,
}

impl Wsl {
    /// How the kernel row names it.
    fn label(&self) -> &'static str {
        match self {
            Wsl::One => "WSL1",
            Wsl::Two => "WSL2",
        }
    }
}

/// Which WSL a kernel release names, if it names one: the release carries `microsoft`, and WSL2's
/// also carries `microsoft-standard` (or `wsl2`), as in `5.15.153.1-microsoft-standard-WSL2`. Any
/// other `microsoft` release is WSL1's, as in `4.4.0-19041-Microsoft`. A stock Linux kernel,
/// including a generic Ubuntu one, carries neither.
fn wsl_kernel(osrelease: &str) -> Option<Wsl> {
    let release = osrelease.to_lowercase();
    if !release.contains("microsoft") {
        return None;
    }
    if release.contains("microsoft-standard") || release.contains("wsl2") {
        Some(Wsl::Two)
    } else {
        Some(Wsl::One)
    }
}

/// [`wsl_kernel`] against this machine's own `/proc/sys/kernel/osrelease`, read the way
/// [`kernel_line`] reads `/etc/os-release`: one file, no probe invented. `None` where the file
/// does not exist, which is everywhere [`runtime::probe_kvm`] has no kvm question for anyway.
fn on_wsl() -> Option<Wsl> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .and_then(|release| wsl_kernel(&release))
}

/// What the failing kvm row says on WSL. On WSL2 a `/dev/kvm` that is simply absent is nested
/// virtualization being off — the WSL2 kernel carries kvm built in, so there is no module to load
/// and the fix lives in Windows. On WSL1 there is no setting at all: the distro has to move to
/// WSL2. Either gets one line carrying the whole fix, in place of the table's `ls -l /dev/kvm`
/// answer. A `/dev/kvm` that exists but refused this user is an ordinary permission problem, and
/// keeps the table's own sentence; `None` says so. Pure, so the branch is decided the same way
/// the tests check it.
fn wsl_kvm_failure(wsl: Option<Wsl>, kvm_exists: bool) -> Option<String> {
    let wsl = wsl?;
    if kvm_exists {
        return None;
    }
    Some(match wsl {
        Wsl::One => {
            "this is WSL1, which cannot run microVMs: convert the distro with `wsl --set-version <distro> 2` from Windows, then enable nested virtualization"
                .to_string()
        }
        Wsl::Two => {
            "/dev/kvm is missing inside WSL2: nested virtualization is off. Add `nestedVirtualization=true` under `[wsl2]` in %UserProfile%\\.wslconfig, run `wsl --shutdown` from Windows, and reopen the shell (needs Windows 11 and a CPU with VT-x or AMD-V)"
                .to_string()
        }
    })
}

/// [`kernel_line`]'s text, pure: the platform, the distribution's own name when there is one, and
/// the WSL note when the kernel is a WSL one — Linux to every other check, but a machine whose
/// kvm answer is Windows's to give, and the row above the kvm one says which WSL.
fn platform_line(os: &str, pretty: Option<&str>, wsl: Option<Wsl>) -> String {
    let mut line = os.to_string();
    if let Some(pretty) = pretty {
        line.push_str(&format!(" ({pretty})"));
    }
    if let Some(wsl) = wsl {
        line.push_str(&format!(" ({})", wsl.label()));
    }
    line
}

/// The platform, plus the distribution's own name when `/etc/os-release` happens to be readable,
/// plus the WSL note. Only those two files: the kernel version needs a probe, and `doctor` must
/// not invent one it did not run.
fn kernel_line() -> String {
    let pretty = std::fs::read_to_string("/etc/os-release").ok().and_then(|release| {
        release
            .lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|pretty| pretty.trim_matches('"').to_string())
    });
    platform_line(std::env::consts::OS, pretty.as_deref(), on_wsl())
}

/// Where the `msb` setting points: the configured value when it is a path, else the first `PATH`
/// entry that holds it. A bare name is how the setting is written and is not a fault; not being on
/// `PATH` either is.
fn resolve_msb(msb: &str) -> Option<PathBuf> {
    let configured = Path::new(msb);
    if configured.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
        return configured.exists().then(|| configured.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|dirs| {
        std::env::split_paths(&dirs)
            .map(|dir| dir.join(msb))
            .find(|candidate| candidate.exists())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The issue's case: a bare ENOSYS from setup must come out naming the check and the command
    /// to run, for every component — a sentence that does not say which thing failed is the bare
    /// errno with more words around it.
    #[test]
    fn the_enosys_entry_names_the_check_and_the_next_command() {
        for component in [Component::MeshControl, Component::MeshNode, Component::MicroVm] {
            let diagnosis = diagnose(component, libc::ENOSYS).expect("ENOSYS is explained");
            let sentence = diagnosis.to_string();
            assert!(
                sentence.starts_with(diagnosis.component),
                "{sentence} should name the component"
            );
            assert!(sentence.contains("The check that settles it:"), "{sentence}");
            assert!(!diagnosis.check.is_empty(), "{sentence} has nothing to check");
            assert!(sentence.ends_with("Next: `colonizer doctor`."), "{sentence}");
        }
    }

    /// The two components share ENOSYS but not its cause: one is the guest kernel, the other this
    /// host's, and a sentence that got that backwards would send the operator to the wrong box.
    #[test]
    fn the_enosys_entry_differs_between_the_microvm_and_the_mesh() {
        let microvm = diagnose(Component::MicroVm, libc::ENOSYS).expect("explained");
        assert!(microvm.check.contains("libkrunfw"), "{}", microvm.check);
        assert!(microvm.check.contains("/dev/kvm"), "{}", microvm.check);
        for component in [Component::MeshControl, Component::MeshNode] {
            let mesh = diagnose(component, libc::ENOSYS).expect("explained");
            assert!(mesh.check.contains("/dev/kvm"), "{}", mesh.check);
            assert!(
                mesh.check.contains("kvm_intel") || mesh.check.contains("KVM"),
                "{}",
                mesh.check
            );
            assert_ne!(mesh.check, microvm.check, "the guest kernel is not this host's");
        }
    }

    /// ENOENT is a missing file, and which file depends on the component: the vendored mesh
    /// binary or the microVM binary, named by the setting that overrides it.
    #[test]
    fn the_enoent_entry_differs_between_the_mesh_binary_and_the_microvm_binary() {
        for component in [Component::MeshControl, Component::MeshNode] {
            let mesh = diagnose(component, libc::ENOENT).expect("explained");
            assert!(mesh.cause.contains("mesh binary"), "{}", mesh.cause);
            assert!(mesh.check.contains("scripts/install.sh"), "{}", mesh.check);
        }
        let microvm = diagnose(Component::MicroVm, libc::ENOENT).expect("explained");
        assert!(microvm.cause.contains("msb"), "{}", microvm.cause);
        assert!(microvm.check.contains("COLONIZER_MSB"), "{}", microvm.check);
    }

    /// A permission refusal is the same whichever way the kernel spells it, and it has to name the
    /// command an operator can run to see for themselves.
    #[test]
    fn a_refused_permission_names_dev_kvm_the_kvm_group_and_the_command() {
        for errno in [libc::EACCES, libc::EPERM] {
            let diagnosis = diagnose(Component::MicroVm, errno).expect("explained");
            assert!(diagnosis.cause.contains("/dev/kvm"), "{}", diagnosis.cause);
            assert!(diagnosis.check.contains("/dev/kvm"), "{}", diagnosis.check);
            assert!(diagnosis.check.contains("kvm group"), "{}", diagnosis.check);
            assert!(diagnosis.check.contains("ls -l /dev/kvm"), "{}", diagnosis.check);
        }
    }

    /// The table is a whitelist. An errno nobody has thought about must leave the caller's own
    /// error alone rather than get a sentence invented for it.
    #[test]
    fn an_errno_the_table_does_not_explain_has_no_diagnosis() {
        assert!(diagnose(Component::MicroVm, libc::EPIPE).is_none());
        assert!(diagnose(Component::MeshNode, libc::EPIPE).is_none());
    }

    /// The two doors have to say the same thing about one errno. A boot failure prints the
    /// [`Diagnosis`] and `doctor` prints `why`, so an edit to a check that changed only one of
    /// them would leave an operator with an error message naming one command and a `doctor` row
    /// naming another — both correct-looking, neither reachable from the other. The pairs are
    /// spelled out so adding a row to the table is a line here as well as in the `match`.
    #[test]
    fn the_doctor_row_and_the_failure_message_carry_the_same_check() {
        for (component, errno) in [
            (Component::MicroVm, libc::ENOSYS),
            (Component::MeshControl, libc::ENOSYS),
            (Component::MeshNode, libc::ENOSYS),
            (Component::MicroVm, libc::ENODEV),
            (Component::MeshControl, libc::ENODEV),
            (Component::MeshNode, libc::ENODEV),
            (Component::MicroVm, libc::EACCES),
            (Component::MeshNode, libc::EPERM),
            (Component::MicroVm, libc::ENOENT),
            (Component::MeshControl, libc::ENOENT),
        ] {
            let diagnosis = diagnose(component, errno).expect("the pair is explained");
            let expected = format!("{}. {}", diagnosis.cause, diagnosis.check);
            let message = why(component, errno);
            assert_eq!(message, expected, "{diagnosis} and the doctor row say different things");
            // The check is the part that must arrive whole: `doctor` drops the component and the
            // `Next:` line, and the runnable command is what an operator has left.
            assert!(message.ends_with(diagnosis.check), "{message} lost its check");
        }
        // And nothing is invented for an errno the table does not explain: the `.unwrap_or_default()`
        // in `why`'s body, which leaves the caller with a line of its own rather than a wrong one.
        assert!(why(Component::MicroVm, libc::EPIPE).is_empty());
    }

    /// `util::exec` folds a child's stderr into the error string, so this is the shape an ENOSYS
    /// actually arrives in when it comes out of `msb` rather than out of a syscall.
    #[test]
    fn a_child_stderr_naming_enosys_is_matched() {
        let stderr = "`msb run --detach` failed (exit status: 1): fatal: landlock_create_ruleset: ENOSYS";
        let diagnosis = diagnose_text(Component::MicroVm, stderr).expect("ENOSYS in the text");
        assert!(diagnosis.to_string().ends_with("Next: `colonizer doctor`."), "{diagnosis}");
        // The same errno spelled out, as a Rust error and as a child both spell it.
        assert!(diagnose_text(Component::MeshNode, "OS error 38 (Function not implemented)").is_some());
        assert!(diagnose_text(Component::MeshControl, "failed to start headscale: ENOSYS").is_some());
        // Case does not decide it.
        assert!(diagnose_text(Component::MeshNode, "os error 38 (function not implemented)").is_some());
    }

    #[test]
    fn unrelated_stderr_has_no_diagnosis() {
        assert!(diagnose_text(Component::MicroVm, "`msb ls` failed (exit status: 1): image not found").is_none());
        assert!(diagnose_text(Component::MeshNode, "").is_none());
    }

    /// Both doors: an `io::Error` that still has its errno, and one that only has the text — which
    /// is what an error that has been folded out of a child's stderr looks like by the time it
    /// reaches here.
    #[test]
    fn explain_reads_the_errno_then_falls_back_to_the_text() {
        let with_errno = io::Error::from_raw_os_error(libc::ENOSYS);
        assert!(
            explain(Component::MicroVm, &with_errno).is_some(),
            "an io::Error carries its errno"
        );
        let text_only = io::Error::other("`msb run` failed (exit status: 1): fatal: ENOSYS");
        assert!(
            explain(Component::MicroVm, &text_only).is_some(),
            "and the text when it has no errno"
        );
        assert!(explain(Component::MicroVm, &io::Error::other("the image pull failed")).is_none());
        // An `anyhow::Error` is not a `std::error::Error` at all, so it comes in by its own door.
        assert!(
            diagnose_text(Component::MicroVm, &anyhow::anyhow!("fatal: ENOSYS").to_string()).is_some(),
            "the shape a wait or a join failure has"
        );
    }

    /// The composed error is what a boot prints: what was attempted, what the process said, and
    /// the check — so an operator gets all three without opening the source.
    #[test]
    fn failed_keeps_what_was_attempted_the_original_error_and_the_diagnosis() {
        let error = failed_anyhow(
            Component::MicroVm,
            "microVM colony-abc failed to boot",
            &anyhow::anyhow!("`msb run --detach` failed (exit status: 1): ENOSYS"),
        );
        let message = format!("{error:#}");
        assert!(message.contains("microVM colony-abc failed to boot"), "{message}");
        assert!(message.contains("exit status: 1"), "the child's own words: {message}");
        assert!(message.contains("Next: `colonizer doctor`"), "{message}");
        // An unexplained failure is passed through untouched rather than dressed up.
        let plain = failed_anyhow(
            Component::MeshNode,
            "harness tailscaled did not start",
            &anyhow::anyhow!("timed out after 20s"),
        );
        assert_eq!(plain.to_string(), "harness tailscaled did not start: timed out after 20s");
        // The errno door, taken from a `spawn` that never got off the ground.
        let refused = failed(
            Component::MeshControl,
            "failed to start headscale",
            &io::Error::from_raw_os_error(libc::EACCES),
        );
        assert!(refused.to_string().contains("kvm group"), "{refused}");
    }

    /// Host-dependent: the answer is an ABI version or an errno — a kernel without landlock says
    /// ENOSYS, a sandboxed one EPERM, and both are legitimate. Only the shape is pinned: a failure
    /// has to stay an `io::Error`, or there is no errno left for `diagnose` to read.
    #[test]
    fn the_landlock_probe_answers_a_version_or_an_io_error() {
        match landlock() {
            Ok(version) => assert!(version >= 1, "ABI version {version}"),
            Err(e) => {
                assert!(
                    e.downcast_ref::<io::Error>().is_some(),
                    "the errno is preserved for diagnose: {e}"
                );
            }
        }
    }

    /// The ENOSYS this module exists for, read on the host side: the kernel `colonizer` is running
    /// on does not implement the syscall. It must not come back in libkrunfw's words — the guest
    /// kernel is a different kernel, and sending an operator to it when the failure was this host's
    /// is the whole defect.
    #[test]
    fn the_host_landlock_enosys_names_the_host_kernel_and_never_libkrunfw() {
        let line = host_landlock_failure(&io::Error::from_raw_os_error(libc::ENOSYS).into())
            .expect("ENOSYS is explained on the host side too");
        assert!(line.contains("host kernel"), "{line}");
        assert!(line.contains("landlock_create_ruleset"), "{line}");
        assert!(!line.contains("libkrunfw"), "this is this host, not the guest: {line}");
        assert!(!line.contains("guest kernel"), "this is this host, not the guest: {line}");
        // The check is still a check, and it is a real one: an operator has to be able to run it.
        assert!(line.contains("CONFIG_SECURITY_LANDLOCK"), "{line}");
        assert!(line.contains("/proc/cmdline"), "{line}");
        // It reads as the line it is, not as a sentence with a `Next:` pointing at `doctor` itself.
        assert!(!line.contains("colonizer doctor"), "{line}");
    }

    /// A refusal is not a KVM permission problem. In a container the syscall is blocked before it
    /// runs, and the `/dev/kvm` sentence would send an operator to fix something that is not wrong.
    #[test]
    fn a_refused_landlock_probe_says_the_policy_refused_it_not_that_kvm_is_wrong() {
        for errno in [libc::EPERM, libc::EACCES] {
            let line = host_landlock_failure(&io::Error::from_raw_os_error(errno).into())
                .expect("a refusal is explained on the host side too");
            assert!(line.contains("refused before it ran"), "{line}");
            assert!(line.contains("seccomp"), "{line}");
            assert!(line.contains("says nothing about whether colonies can boot"), "{line}");
            assert!(!line.contains("/dev/kvm"), "KVM is not what refused it: {line}");
            assert!(!line.contains("libkrunfw"), "this is this host, not the guest: {line}");
            assert!(
                !line.contains("colonizer doctor"),
                "it should not point at the running command: {line}"
            );
        }
    }

    /// Anything the host side has no reading for keeps the caller's own error: a wrong explanation
    /// is worse than none, and this is the same rule the table follows.
    #[test]
    fn a_landlock_errno_the_host_side_does_not_explain_falls_through() {
        let unexplained = io::Error::from_raw_os_error(libc::ENODEV).into();
        assert!(
            host_landlock_failure(&unexplained).is_none(),
            "the caller falls through to failed_anyhow for this one"
        );
        // An error with no errno at all is the same case.
        assert!(host_landlock_failure(&anyhow::anyhow!("landlock is a Linux feature")).is_none());
    }

    /// No assets directory is the root cause of every missing vendored file, so the line names the
    /// directory and the way to build it — not a mesh binary, which is a consequence of it.
    #[test]
    fn missing_assets_names_the_directory_and_install_sh_not_a_mesh_binary() {
        for line in [assets_missing(None), assets_missing(Some(Path::new("/opt/colonizer/app")))] {
            assert!(line.contains("scripts/install.sh"), "{line}");
            assert!(
                !line.contains("mesh binary"),
                "the binary is a consequence, not the cause: {line}"
            );
            assert!(
                !line.contains("colonizer doctor"),
                "it should not point at the running command: {line}"
            );
            assert!(line.contains("assets"), "{line}");
        }
        // The setting that named a path is quoted back, so the operator knows which one was wrong.
        let named = assets_missing(Some(Path::new("/opt/colonizer/app")));
        assert!(named.starts_with("/opt/colonizer/app is not a directory"), "{named}");
        assert!(
            assets_missing(None).contains("could not be found"),
            "{}",
            assets_missing(None)
        );
    }

    /// The vendored mesh binaries, named as `binaries_present` looks for them.
    #[test]
    fn the_mesh_binary_names_match_the_paths_binaries_present_checks() {
        let assets = Path::new("/nonexistent-assets");
        let missing: Vec<&str> = MESH_BINARIES
            .iter()
            .filter(|(rel, _)| !assets.join(rel).exists())
            .map(|(_, name)| *name)
            .collect();
        assert_eq!(missing, ["headscale", "tailscale", "tailscaled"]);
        assert!(!mesh::binaries_present(assets));
    }

    /// A WSL kernel names itself in its release; WSL2's also names the standard build, and any
    /// other `microsoft` release is WSL1's. A stock Linux kernel names neither, whichever
    /// distribution named it.
    #[test]
    fn a_wsl_kernel_release_names_microsoft_and_a_stock_linux_one_does_not() {
        assert_eq!(wsl_kernel("5.15.153.1-microsoft-standard-WSL2"), Some(Wsl::Two));
        assert_eq!(wsl_kernel("4.4.0-19041-Microsoft"), Some(Wsl::One));
        assert_eq!(wsl_kernel("6.12.109"), None);
        assert_eq!(wsl_kernel("6.8.0-45-generic"), None);
    }

    /// On WSL2 an absent `/dev/kvm` is nested virtualization, and the one line carries the whole
    /// fix: the key, the file it lives in, the restart. On WSL1 there is no setting — the line
    /// says the distro has to convert. A `/dev/kvm` that exists but refuses the user stays the
    /// table's ordinary permission problem, as does anything off WSL.
    #[test]
    fn the_wsl_kvm_lines_name_the_fix_in_one_line() {
        let two = wsl_kvm_failure(Some(Wsl::Two), false).expect("a missing /dev/kvm on WSL2 is explained");
        assert!(!two.contains('\n'), "one line: {two}");
        assert!(two.contains("nestedVirtualization=true"), "{two}");
        assert!(two.contains(".wslconfig"), "{two}");
        assert!(two.contains("wsl --shutdown"), "{two}");
        let one = wsl_kvm_failure(Some(Wsl::One), false).expect("a missing /dev/kvm on WSL1 is explained");
        assert!(!one.contains('\n'), "one line: {one}");
        assert!(one.contains("WSL1"), "{one}");
        assert!(one.contains("cannot run microVMs"), "{one}");
        assert!(one.contains("wsl --set-version"), "{one}");
        assert!(
            wsl_kvm_failure(Some(Wsl::Two), true).is_none(),
            "a permission problem stays the table's"
        );
        assert!(wsl_kvm_failure(None, false).is_none(), "not WSL, not this line");
    }

    /// The kernel row names the WSL it found, so an operator reading `linux (Ubuntu) (WSL2)` knows
    /// the kvm row below it is Windows's to fix — and one reading `(WSL1)` knows no setting will
    /// do — without a WSL kernel to ask.
    #[test]
    fn the_kernel_line_notes_the_wsl_and_keeps_the_distribution_name() {
        assert_eq!(
            platform_line("linux", Some("Ubuntu 24.04 LTS"), Some(Wsl::Two)),
            "linux (Ubuntu 24.04 LTS) (WSL2)"
        );
        assert_eq!(platform_line("linux", None, Some(Wsl::One)), "linux (WSL1)");
        assert_eq!(platform_line("linux", None, None), "linux");
        assert_eq!(platform_line("macos", Some("macOS 15.0"), None), "macos (macOS 15.0)");
    }
}
