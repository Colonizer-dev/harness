//! In-guest process hardening for the agent runner child (issue #301). The child runs the agent
//! module's untrusted code as root inside a single-tenant microVM, so before exec it gets: the
//! capability bounding set trimmed (for uid 0, caps after execve are exactly the bounding set, so a
//! drop here strips the agent and everything it spawns, while chown/dac_override/setuid/setgid/
//! net_raw survive for package managers), RLIMIT_CORE 0 (no core dumps), PR_SET_NO_NEW_PRIVS and a
//! hand-assembled classic-BPF seccomp denylist that turns the dangerous calls — kernel module/BPF/
//! io_uring/userfaultfd/mount/namespace/ptrace territory — into ordinary EPERM tool failures
//! instead of kills.
//!
//! Everything is assembled before the fork ([`Hardening::prepare`]); the pre_exec closure only
//! makes async-signal-safe raw syscalls and a failure in any step fails the spawn (fail closed).
//! agentd itself and the PTY shell (the human's terminal) are deliberately not filtered — only the
//! runner child — and agentd's own non-dumpability and core limit are set at startup
//! ([`self_guard`]) so a capability-less agent cannot read the daemon's memory or environ.

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod imp {
    use serde_json::{Value, json};
    use std::io;

    /// What a matching rule returns: deny outright with this errno, or deny only when the low 32
    /// bits of argument `arg` equal `value` / share any bit with `mask` — the rest of that
    /// syscall's surface passes. Denials surface as ordinary tool failures, never as kills.
    enum Action {
        Errno(u32),
        ArgEq(u8, u32),
        ArgAny(u8, u32),
    }

    /// One denylist entry: the syscall by name (for the profile) and number (for the program), the
    /// action, and for arg-gated rules the condition as it should read in the profile JSON.
    struct Rule {
        name: &'static str,
        nr: libc::c_long,
        action: Action,
        when: &'static str,
    }

    // prctl(2) options, seccomp(2) constants and the TIOCSTI/TIOCLINUX ioctls that libc 0.2 only
    // exports for android/fuchsia (or not at all); asm-generic ioctl numbers are ABI-stable.
    const PR_SET_DUMPABLE: libc::c_long = 4;
    const PR_SET_NO_NEW_PRIVS: libc::c_long = 38;
    const PR_CAPBSET_DROP: libc::c_long = 24;
    const SECCOMP_SET_MODE_FILTER: libc::c_long = 1;
    const SECCOMP_FILTER_FLAG_LOG: libc::c_ulong = 1 << 1;
    const TIOCSTI: u32 = 0x5412;
    const TIOCLINUX: u32 = 0x541c;

    /// The clone(2) namespace flags. CLONE_NEWTIME (0x80) is excluded: it overlaps CSIGNAL, so
    /// clone cannot carry it anyway.
    const NAMESPACE_FLAGS: u32 = 0x7e02_0000; // NEW{NS,CGROUP,UTS,IPC,USER,PID,NET}

    /// Capabilities dropped from the bounding set before exec: kernel module/BPF/perf/port-I/O
    /// territory, mounts, ptrace, audit, syslog, quotas. Two are walls on purpose: no CAP_SYS_RESOURCE
    /// keeps RLIMIT_CORE=0 from being raised again, and no CAP_SYS_PTRACE keeps a non-dumpable
    /// agentd's /proc/<pid>/{environ,mem} closed to the agent.
    #[rustfmt::skip]
    const DROPPED_CAPS: &[libc::c_ulong] = &[
        9 /* LINUX_IMMUTABLE */, 12 /* NET_ADMIN */, 16 /* SYS_MODULE */, 17 /* SYS_RAWIO */,
        19 /* SYS_PTRACE */, 20 /* SYS_PACCT */, 21 /* SYS_ADMIN */, 22 /* SYS_BOOT */,
        24 /* SYS_RESOURCE */, 25 /* SYS_TIME */, 26 /* SYS_TTY_CONFIG */, 27 /* MKNOD */,
        30 /* AUDIT_CONTROL */, 32 /* MAC_OVERRIDE */, 33 /* MAC_ADMIN */, 34 /* SYSLOG */,
        35 /* WAKE_ALARM */, 36 /* BLOCK_SUSPEND */, 38 /* PERFMON */, 39 /* BPF */,
        40 /* CHECKPOINT_RESTORE */,
    ];

    /// The plain EPERM denylist. Numbers 425+ are shared by x86_64 and aarch64, so statmount/
    /// listmount (kexec_file_load on aarch64 musl, not in libc 0.2) are literals; the x86_64-only
    /// legacy calls are in [`X86_64_DENY`]. fanotify_init is denied on judgement: a root-only
    /// security-monitoring surface no build agent needs.
    #[rustfmt::skip]
    const DENY: &[(&str, libc::c_long)] = &[
        ("io_uring_setup", libc::SYS_io_uring_setup), ("io_uring_enter", libc::SYS_io_uring_enter),
        ("io_uring_register", libc::SYS_io_uring_register), ("userfaultfd", libc::SYS_userfaultfd),
        ("bpf", libc::SYS_bpf), ("perf_event_open", libc::SYS_perf_event_open),
        ("unshare", libc::SYS_unshare), ("setns", libc::SYS_setns),
        ("pivot_root", libc::SYS_pivot_root), ("chroot", libc::SYS_chroot),
        ("mount", libc::SYS_mount), ("umount2", libc::SYS_umount2),
        ("open_tree", libc::SYS_open_tree), ("move_mount", libc::SYS_move_mount),
        ("fsopen", libc::SYS_fsopen), ("fsconfig", libc::SYS_fsconfig),
        ("fsmount", libc::SYS_fsmount), ("fspick", libc::SYS_fspick),
        ("mount_setattr", libc::SYS_mount_setattr), ("statmount", 457),
        ("listmount", 458), ("ptrace", libc::SYS_ptrace),
        ("process_vm_readv", libc::SYS_process_vm_readv), ("process_vm_writev", libc::SYS_process_vm_writev),
        ("pidfd_getfd", libc::SYS_pidfd_getfd), ("kcmp", libc::SYS_kcmp),
        ("add_key", libc::SYS_add_key), ("request_key", libc::SYS_request_key),
        ("keyctl", libc::SYS_keyctl), ("init_module", libc::SYS_init_module),
        ("finit_module", libc::SYS_finit_module), ("delete_module", libc::SYS_delete_module),
        ("kexec_load", libc::SYS_kexec_load), ("kexec_file_load", SYS_KEXEC_FILE_LOAD),
        ("reboot", libc::SYS_reboot), ("swapon", libc::SYS_swapon),
        ("swapoff", libc::SYS_swapoff), ("syslog", libc::SYS_syslog),
        ("acct", libc::SYS_acct), ("quotactl", libc::SYS_quotactl),
        ("open_by_handle_at", libc::SYS_open_by_handle_at), ("fanotify_init", libc::SYS_fanotify_init),
    ];

    /// x86_64-only legacy surface: the old umount, port I/O, and lookup_dcookie (gone from the
    /// kernel's tables in 6.11 anyway). On aarch64 these calls do not exist.
    #[cfg(target_arch = "x86_64")]
    #[rustfmt::skip]
    const X86_64_DENY: &[(&str, libc::c_long)] = &[("umount", 22), ("iopl", 172), ("ioperm", 173), ("lookup_dcookie", 212)];

    #[cfg(target_arch = "x86_64")]
    const SYS_KEXEC_FILE_LOAD: libc::c_long = libc::SYS_kexec_file_load;
    #[cfg(target_arch = "aarch64")]
    const SYS_KEXEC_FILE_LOAD: libc::c_long = 294; // kexec_file_load on aarch64; absent from musl libc 0.2

    // seccomp_data.arch for the native ABI: AUDIT_ARCH_<arch> = EM_<arch> | 0x4000_0000.
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e; // EM_X86_64
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7; // EM_AARCH64

    // Classic BPF opcodes (linux/filter.h); libc 0.2 ships the sock_filter struct but not these.
    // seccomp_data: nr at offset 0, arch at 4, args[i] low 32 bits at 16 + 8*i.
    const BPF_LD_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const BPF_JEQ: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const BPF_JSET: u16 = 0x45; // BPF_JMP | BPF_JSET | BPF_K
    const BPF_RET: u16 = 0x06; // BPF_RET | BPF_K
    const OFF_NR: u32 = 0;
    const OFF_ARCH: u32 = 4;
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))] // only the x86_64 x32 check jumps numerically
    const BPF_JGE: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
    const fn off_arg(arg: u8) -> u32 {
        16 + 8 * arg as u32
    }

    fn rule(name: &'static str, nr: libc::c_long, action: Action, when: &'static str) -> Rule {
        Rule { name, nr, action, when }
    }

    fn rules() -> Vec<Rule> {
        let eperm = |&(name, nr)| rule(name, nr, Action::Errno(libc::EPERM as u32), "");
        let mut rules: Vec<Rule> = DENY.iter().map(eperm).collect();
        #[cfg(target_arch = "x86_64")]
        rules.extend(X86_64_DENY.iter().map(eperm));
        // clone3 gets ENOSYS: its flags hide in a struct seccomp cannot read, and glibc/Go fall
        // back to plain clone. Plain clone is only denied when it creates a namespace — mknod is
        // not denied at all (FIFOs are legitimate build tooling; device nodes are covered by the
        // dropped CAP_MKNOD). execve resets dumpable itself, so the prctl gate only keeps the
        // agent from flipping it back on; the real wall is RLIMIT_CORE=0 plus the dropped
        // CAP_SYS_RESOURCE/CAP_SYS_PTRACE. The ioctl gates stop terminal injection into the
        // human's PTY.
        rules.extend([
            rule("clone3", libc::SYS_clone3, Action::Errno(libc::ENOSYS as u32), ""),
            rule(
                "clone",
                libc::SYS_clone,
                Action::ArgAny(0, NAMESPACE_FLAGS),
                "arg0 & CLONE_NEW{NS,CGROUP,UTS,IPC,USER,PID,NET}",
            ),
            rule(
                "prctl",
                libc::SYS_prctl,
                Action::ArgEq(0, PR_SET_DUMPABLE as u32),
                "arg0 == PR_SET_DUMPABLE",
            ),
            rule("ioctl", libc::SYS_ioctl, Action::ArgEq(1, TIOCSTI), "arg1 == TIOCSTI"),
            rule("ioctl", libc::SYS_ioctl, Action::ArgEq(1, TIOCLINUX), "arg1 == TIOCLINUX"),
        ]);
        rules
    }

    /// Compiles the rules into classic BPF, default ALLOW: an arch check first (a foreign ABI like
    /// i386's int 0x80 uses different numbers — deny it outright; on x86_64 the x32 high bit too),
    /// then one jeq per syscall, arg-gated rules loading their argument. A matching syscall falls
    /// into its deny block; a non-match skips past it and on to the next rule.
    fn assemble(rules: &[Rule]) -> Vec<libc::sock_filter> {
        let mut f = Vec::with_capacity(rules.len() * 2 + 8);
        let insn = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
        let ret = |errno: u32| insn(BPF_RET, 0, 0, 0x0005_0000 | errno); // SECCOMP_RET_ERRNO
        let deny = ret(libc::EPERM as u32);
        f.push(insn(BPF_LD_ABS, 0, 0, OFF_ARCH));
        f.push(insn(BPF_JEQ, 1, 0, AUDIT_ARCH)); // native arch skips the foreign-ABI EPERM below
        f.push(deny);
        #[cfg(target_arch = "x86_64")]
        {
            f.push(insn(BPF_LD_ABS, 0, 0, OFF_NR));
            f.push(insn(BPF_JGE, 0, 1, 0x4000_0000)); // non-x32 numbers skip the EPERM below
            f.push(deny);
        }
        for rule in rules {
            f.push(insn(BPF_LD_ABS, 0, 0, OFF_NR));
            let branch = f.len();
            f.push(insn(BPF_JEQ, 0, 0, rule.nr as u32));
            f[branch].jf = match rule.action {
                Action::Errno(errno) => {
                    f.push(ret(errno));
                    1
                }
                Action::ArgEq(arg, value) => {
                    f.push(insn(BPF_LD_ABS, 0, 0, off_arg(arg)));
                    f.push(insn(BPF_JEQ, 0, 1, value)); // match falls into the EPERM below
                    f.push(deny);
                    3
                }
                Action::ArgAny(arg, mask) => {
                    f.push(insn(BPF_LD_ABS, 0, 0, off_arg(arg)));
                    f.push(insn(BPF_JSET, 0, 1, mask)); // any bit falls into the EPERM below
                    f.push(deny);
                    3
                }
            };
        }
        f.push(insn(BPF_RET, 0, 0, 0x7fff_0000)); // SECCOMP_RET_ALLOW
        f
    }

    /// FNV-1a 64 over the assembled program's bytes: a stable fingerprint for the startup log,
    /// `--seccomp-profile` and docs, so any profile change is visible at a glance.
    fn fnv64(filter: &[libc::sock_filter]) -> String {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        assert_eq!(std::mem::size_of::<libc::sock_filter>(), 8);
        // SAFETY: sock_filter is {code: u16, jt: u8, jf: u8, k: u32} — no padding, size 8 as
        // asserted — so the filter's memory is a valid `[u8]` of len*8 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(filter.as_ptr().cast::<u8>(), filter.len() * 8) };
        let mut hash = OFFSET;
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(PRIME);
        }
        format!("{hash:016x}")
    }

    /// One runner child's hardening, assembled before the fork so the pre_exec closure has nothing
    /// left to allocate.
    pub struct Hardening {
        filter: Vec<libc::sock_filter>,
        rule_count: usize,
    }

    impl Hardening {
        pub fn prepare() -> Self {
            let rules = rules();
            let rule_count = rules.len();
            Self {
                filter: assemble(&rules),
                rule_count,
            }
        }

        /// The startup log line: e.g. `seccomp denylist 51 rules fnv64=…, caps dropped 21, core dumps off`.
        pub fn describe(&self) -> String {
            let caps = if unsafe { libc::geteuid() } == 0 {
                DROPPED_CAPS.len()
            } else {
                0
            };
            format!(
                "seccomp denylist {} rules fnv64={}, caps dropped {caps}, core dumps off",
                self.rule_count,
                fnv64(&self.filter)
            )
        }

        /// The `pre_exec` closure for the runner Command: raw syscalls only, nothing allocated. A
        /// failing step fails the spawn through Command::spawn's existing error path.
        pub fn guard(self) -> impl FnMut() -> io::Result<()> + Send + Sync {
            move || apply(&self.filter)
        }
    }

    /// RLIMIT_CORE 0/0 — the one step the runner child (`apply`) and agentd itself
    /// (`self_guard`) share: no core file, so a crash cannot embed whoever's environment.
    fn zero_core_limit() -> io::Result<()> {
        let zero = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The hardening itself, in order: caps, core limit, no_new_privs, then the filter last (so it
    /// cannot block its own setup). Safe to run between fork and exec.
    fn apply(filter: &[libc::sock_filter]) -> io::Result<()> {
        unsafe {
            // For uid 0 the caps after execve are exactly the bounding set (execing as root makes
            // the file capability masks all-ones), so dropping these here strips them from the
            // agent and every descendant — no capset needed. As non-root (CI) the drop is skipped:
            // a non-root bounding set holds nothing to lose.
            if libc::geteuid() == 0 {
                for cap in DROPPED_CAPS {
                    if libc::syscall(libc::SYS_prctl, PR_CAPBSET_DROP, *cap) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            // No core dumps (the rlimit is the wall; the dropped CAP_SYS_RESOURCE keeps it there).
            zero_core_limit()?;
            // The precondition for installing a filter unprivileged, and cheap insurance for a
            // root child: execve can no longer gain privileges through setuid files.
            if libc::syscall(libc::SYS_prctl, PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if !filter.is_empty() {
                let program = libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_ptr() as *mut libc::sock_filter,
                };
                // Denials land in the kernel's audit stream where one exists; kernels that reject
                // the flag (EINVAL) get the identical filter without it.
                if libc::syscall(libc::SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_LOG, &program) == -1
                    && libc::syscall(libc::SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program) == -1
                {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }

    /// The profile as data for `--seccomp-profile` (scripts/seccomp-evidence.sh, docs): the denied
    /// syscalls, the non-EPERM overrides and the arg-gated rules, plus the program fingerprint.
    pub fn profile_json() -> Value {
        let rule_list = rules();
        let mut deny: Vec<&str> = rule_list.iter().map(|r| r.name).collect();
        deny.sort_unstable();
        deny.dedup();
        let errno_overrides: Vec<Value> = rule_list
            .iter()
            .filter(|r| matches!(r.action, Action::Errno(e) if e == libc::ENOSYS as u32))
            .map(|r| json!({"syscall": r.name, "errno": "ENOSYS"}))
            .collect();
        let conditional: Vec<Value> = rule_list
            .iter()
            .filter(|r| !r.when.is_empty())
            .map(|r| json!({"syscall": r.name, "when": r.when, "errno": "EPERM"}))
            .collect();
        json!({
            "arch": std::env::consts::ARCH,
            "fingerprint": fnv64(&assemble(&rule_list)),
            "deny": deny,
            "errno_overrides": errno_overrides,
            "conditional": conditional,
        })
    }

    /// Applies the runner-child hardening to this process in place, returning its description;
    /// `--exec-hardened` execs the requested command right after, so what it leaves behind is
    /// exactly what a spawn's pre_exec would leave in the child.
    pub fn apply_self() -> io::Result<String> {
        let hardening = Hardening::prepare();
        apply(&hardening.filter)?;
        Ok(hardening.describe())
    }

    /// agentd's own startup hygiene, before anything is spawned: non-dumpable and core-limit zero,
    /// so a capability-less runner cannot read the daemon's memory or environ out of /proc
    /// (hidepid, applied by boot.sh, keeps the directory listing closed). Not a seccomp subject:
    /// agentd serves the human's PTY. Failures are worth reporting but not fatal — this is hygiene
    /// around the real wall, which is the runner child's filter.
    pub fn self_guard() -> io::Result<()> {
        zero_core_limit()?;
        if unsafe { libc::syscall(libc::SYS_prctl, PR_SET_DUMPABLE, 0, 0, 0, 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::process::CommandExt;

        const PR_GET_NO_NEW_PRIVS: libc::c_long = 39;
        // A libtest substring filter, not --exact: the harness names tests by module path, which
        // module_path!() spells with the crate prefix and would never match.
        const PROBE: &str = "probe_hardened_child";
        const PROBE_ENV: &str = "COLONIZER_AGENTD_HARDENED_PROBE";

        /// Errno a denied raw syscall surfaces, asserting it was denied at all.
        fn errno_of(call: impl FnOnce() -> libc::c_long) -> i32 {
            assert_eq!(call(), -1, "expected the syscall to be denied");
            std::io::Error::last_os_error().raw_os_error().unwrap()
        }

        #[test]
        fn program_is_well_formed_bpf() {
            let filter = assemble(&rules());
            assert!(filter.len() > 8 && filter.len() < 4096, "{} instructions", filter.len());
            // Starts with the arch check: load seccomp_data.arch, native arch skips the EPERM ret.
            assert_eq!((filter[0].code, filter[0].k), (BPF_LD_ABS, OFF_ARCH));
            assert_eq!(
                (filter[1].code, filter[1].jt, filter[1].jf, filter[1].k),
                (BPF_JEQ, 1, 0, AUDIT_ARCH)
            );
            // Every jump lands inside the program, and it ends in the default allow.
            for (i, step) in filter.iter().enumerate() {
                if [BPF_JEQ, BPF_JGE, BPF_JSET].contains(&step.code) {
                    let (jt, jf) = (i + 1 + step.jt as usize, i + 1 + step.jf as usize);
                    assert!(jt < filter.len() && jf < filter.len(), "jump out of range at insn {i}");
                }
            }
            let last = filter.last().unwrap();
            assert_eq!((last.code, last.k), (BPF_RET, 0x7fff_0000));
        }

        #[test]
        fn denylist_covers_the_classes_that_matter() {
            // One representative per class; the full table is the data above and `--seccomp-profile` output.
            let names: Vec<&str> = rules().iter().map(|r| r.name).collect();
            for class in [
                "io_uring_setup",
                "userfaultfd",
                "bpf",
                "perf_event_open", // async kernel interfaces
                "unshare",
                "setns",
                "mount",
                "pivot_root", // isolation
                "ptrace",
                "keyctl",
                "init_module",
                "reboot", // the rest of the estate
            ] {
                assert!(names.contains(&class), "{class} must be denied");
            }
            let rule_list = rules();
            let clone3 = rule_list.iter().find(|r| r.name == "clone3").unwrap();
            assert!(
                matches!(clone3.action, Action::Errno(e) if e == libc::ENOSYS as u32),
                "clone3 must return ENOSYS so glibc and Go fall back to clone"
            );
            assert_eq!(
                rule_list.iter().filter(|r| r.name == "clone").count(),
                1,
                "exactly one clone rule"
            );
        }

        #[test]
        fn fingerprint_moves_when_the_program_changes() {
            let filter = assemble(&rules());
            assert_eq!(fnv64(&filter).len(), 16, "fnv64 prints as 16 hex chars");
            let mut shorter = rules();
            shorter.pop();
            assert_ne!(
                fnv64(&filter),
                fnv64(&assemble(&shorter)),
                "any rule change must move the fingerprint"
            );
        }

        /// Runs the program against a synthetic seccomp_data, the way the kernel would.
        fn run(filter: &[libc::sock_filter], nr: u32, arch: u32, args: [u64; 6]) -> u32 {
            let (mut pc, mut a) = (0usize, 0u32);
            loop {
                let step = &filter[pc];
                match step.code {
                    BPF_LD_ABS => {
                        a = match step.k {
                            OFF_NR => nr,
                            OFF_ARCH => arch,
                            offset => {
                                let arg = ((offset - 16) / 8) as usize;
                                if (offset - 16) % 8 == 0 {
                                    args[arg] as u32
                                } else {
                                    (args[arg] >> 32) as u32
                                }
                            }
                        };
                        pc += 1;
                    }
                    BPF_JEQ => pc += 1 + if a == step.k { step.jt } else { step.jf } as usize,
                    BPF_JGE => pc += 1 + if a >= step.k { step.jt } else { step.jf } as usize,
                    BPF_JSET => pc += 1 + if a & step.k != 0 { step.jt } else { step.jf } as usize,
                    BPF_RET => return step.k,
                    code => panic!("unexpected opcode {code:#x}"),
                }
            }
        }

        /// The whole point of the profile: denials deny with the documented errno, and everything
        /// else — including the arg-gated syscalls' benign calls — reaches ALLOW.
        #[test]
        fn program_semantics_deny_selected_allow_the_rest() {
            const ALLOW: u32 = 0x7fff_0000;
            let filter = assemble(&rules());
            let denied = |k: u32, want: i32| {
                assert_eq!(k & 0xffff_0000, 0x0005_0000, "expected ERRNO, got {k:#x}");
                assert_eq!(k & 0xffff, want as u32);
            };
            assert_eq!(
                run(&filter, libc::SYS_getpid as u32, AUDIT_ARCH, [0; 6]),
                ALLOW,
                "getpid passes"
            );
            denied(run(&filter, libc::SYS_mount as u32, AUDIT_ARCH, [0; 6]), libc::EPERM);
            denied(run(&filter, libc::SYS_io_uring_setup as u32, AUDIT_ARCH, [0; 6]), libc::EPERM);
            denied(run(&filter, 39, 0x0000_0003, [0; 6]), libc::EPERM); // i386 numbers must not match ours
            #[cfg(target_arch = "x86_64")]
            denied(run(&filter, 39 | 0x4000_0000, AUDIT_ARCH, [0; 6]), libc::EPERM); // x32 ABI bit set
            // clone with a plain signal passes, with a namespace flag it is denied.
            assert_eq!(run(&filter, libc::SYS_clone as u32, AUDIT_ARCH, [17, 0, 0, 0, 0, 0]), ALLOW);
            denied(
                run(&filter, libc::SYS_clone as u32, AUDIT_ARCH, [0x1000_0000, 0, 0, 0, 0, 0]),
                libc::EPERM,
            );
            denied(run(&filter, libc::SYS_clone3 as u32, AUDIT_ARCH, [0; 6]), libc::ENOSYS);
            // prctl: only PR_SET_DUMPABLE is gated; PR_SET_NAME (15, shares its bit) must pass.
            denied(
                run(&filter, libc::SYS_prctl as u32, AUDIT_ARCH, [4, 0, 0, 0, 0, 0]),
                libc::EPERM,
            );
            assert_eq!(run(&filter, libc::SYS_prctl as u32, AUDIT_ARCH, [15, 0, 0, 0, 0, 0]), ALLOW);
            // ioctl: only the two terminal-injection commands are gated; TCGETS must pass.
            denied(
                run(&filter, libc::SYS_ioctl as u32, AUDIT_ARCH, [0, 0x5412, 0, 0, 0, 0]),
                libc::EPERM,
            );
            assert_eq!(
                run(&filter, libc::SYS_ioctl as u32, AUDIT_ARCH, [0, 0x5401, 0, 0, 0, 0]),
                ALLOW
            );
        }

        /// The child half of `spawning_through_guard_hardens_the_child`: it runs inside a child
        /// that came through the exact closure runner.rs installs, so every assertion here is what
        /// a real agent experiences. Env-gated so a plain `cargo test` only runs it as that child.
        #[test]
        fn probe_hardened_child() {
            if std::env::var_os(PROBE_ENV).is_none() {
                return;
            }
            let null = std::ptr::null::<u8>();
            assert_eq!(
                errno_of(|| unsafe { libc::syscall(libc::SYS_io_uring_setup, 4, null) }),
                libc::EPERM
            );
            assert_eq!(errno_of(|| unsafe { libc::syscall(libc::SYS_bpf, 0, null, 0) }), libc::EPERM);
            assert_eq!(
                errno_of(|| unsafe { libc::syscall(libc::SYS_unshare, NAMESPACE_FLAGS) }),
                libc::EPERM
            );
            assert_eq!(
                errno_of(|| unsafe { libc::syscall(libc::SYS_mount, null, null, null, 0, null) }),
                libc::EPERM
            );
            assert_eq!(
                errno_of(|| unsafe { libc::syscall(libc::SYS_ptrace, libc::PTRACE_TRACEME, 0, 0, 0) }),
                libc::EPERM
            );
            assert_eq!(errno_of(|| unsafe { libc::syscall(libc::SYS_clone3, null, 0) }), libc::ENOSYS);
            assert_eq!(
                errno_of(|| unsafe { libc::syscall(libc::SYS_prctl, PR_SET_DUMPABLE, 1, 0, 0, 0) }),
                libc::EPERM
            );
            assert_eq!(
                errno_of(|| unsafe { libc::syscall(libc::SYS_ioctl, 0, TIOCSTI as libc::c_ulong, 0) }),
                libc::EPERM
            );

            // The rlimits and no_new_privs actually landed.
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) }, 0);
            assert_eq!((limit.rlim_cur, limit.rlim_max), (0, 0), "core dumps must be off");
            assert_eq!(unsafe { libc::syscall(libc::SYS_prctl, PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) }, 1);
            // As root, the bounding set lost the dangerous caps but kept the package-manager ones.
            if unsafe { libc::geteuid() } == 0 {
                let status = std::fs::read_to_string("/proc/self/status").unwrap();
                let eff = status.lines().find_map(|l| l.strip_prefix("CapEff:")).unwrap();
                let eff = u64::from_str_radix(eff.trim(), 16).unwrap();
                assert_eq!(eff & (1 << 21), 0, "CAP_SYS_ADMIN must not survive exec");
                assert_eq!(eff & (1 << 19), 0, "CAP_SYS_PTRACE must not survive exec");
                assert_ne!(eff & 1, 0, "CAP_CHOWN is kept on purpose for package managers");
            }

            // Normal work still works: processes, files, threads (threads go through clone3 → the
            // ENOSYS override → fallback clone; the fork proves plain clone still passes).
            assert!(unsafe { libc::getpid() } > 0);
            let path = std::env::temp_dir().join(format!("agentd-harden-probe-{}", std::process::id()));
            std::fs::write(&path, b"ok").unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"ok");
            let _ = std::fs::remove_file(&path);
            assert_eq!(std::thread::spawn(|| 40 + 2).join().unwrap(), 42);
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork must still work");
            if pid == 0 {
                unsafe { libc::_exit(7) };
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 7);
        }

        /// Spawns this same test binary through the real `guard` closure and runs the probe inside
        /// it. Root here, non-root on CI — both must pass. Asserting `1 passed` (not just libtest's
        /// `ok`, which a `0 passed` run prints too) is what keeps a mistyped filter from turning
        /// this into a vacuous pass.
        #[test]
        fn spawning_through_guard_hardens_the_child() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([PROBE, "--nocapture"]).env(PROBE_ENV, "1");
            unsafe { command.pre_exec(Hardening::prepare().guard()) };
            let output = command.output().expect("spawn with hardening must succeed");
            let say = format!(
                "--- stdout\n{}--- stderr\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "hardened child failed:\n{say}");
            let ran = String::from_utf8_lossy(&output.stdout);
            assert!(
                ran.contains(PROBE) && ran.contains("1 passed"),
                "the probe did not run:\n{say}"
            );
        }
    }
}

/// Off Linux (macOS dev builds of the workspace) or off the supported arches, everything degrades
/// to a no-op: the crate only ships into Linux colonies, but it must still compile everywhere.
#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod imp {
    use serde_json::{Value, json};
    use std::io;

    pub struct Hardening;

    impl Hardening {
        pub fn prepare() -> Self {
            Hardening
        }
        pub fn describe(&self) -> String {
            format!("unavailable on {}/{}", std::env::consts::OS, std::env::consts::ARCH)
        }
        pub fn guard(self) -> impl FnMut() -> io::Result<()> + Send + Sync {
            || Ok(())
        }
    }

    pub fn profile_json() -> Value {
        json!({"arch": std::env::consts::ARCH, "os": std::env::consts::OS, "fingerprint": null, "deny": []})
    }

    pub fn apply_self() -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "hardening requires Linux on x86_64/aarch64",
        ))
    }

    pub fn self_guard() -> io::Result<()> {
        Ok(())
    }
}

pub use imp::{Hardening, apply_self, profile_json, self_guard};
