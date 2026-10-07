//! Sealing the session token out of the guest's reach (issue #640). agentd needs the bearer token
//! only in memory, to compare requests against it; nothing else in the VM must read it — a runner
//! that could would open `ws://127.0.0.1:7070/v1/pty` and get the human's unfiltered root shell,
//! shedding the profile [crate::harden] applies to it. So when started with `--seal-token`, right
//! after the token is read and before the listener binds or anything is spawned, agentd covers the
//! token path with a read-only bind of `/dev/null`: every later read of the path in the guest — the
//! runner's included — sees an empty file. Fail closed: a seal that cannot be applied, or that does
//! not verify, is a startup error that names the path, never a warning.
//!
//! The runner cannot undo the seal: `harden.rs` denies it the mount family (`mount`, `umount2`,
//! `open_tree`, ...) and `CAP_SYS_ADMIN`, and denies `unshare`/`setns`, so it cannot reach a
//! namespace without the seal either.

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod imp {
    use std::{ffi::CString, io, path::Path};

    /// Covers `path` with a bind of `/dev/null` remounted read-only, then verifies the seal by
    /// reading the path back the way any other process would: it must now read as empty.
    pub fn seal_token_path(path: &Path) -> io::Result<()> {
        let target = CString::new(path.as_os_str().as_encoded_bytes())?;
        let source = b"/dev/null\0";
        // SAFETY: both arguments are NUL-terminated C strings; mount(2) keeps nothing it is handed.
        if unsafe {
            libc::mount(
                source.as_ptr().cast(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above; the remount restates this bind's own flags, read-only.
        if unsafe {
            libc::mount(
                std::ptr::null(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        // A mount that succeeded but did not cover the path (a raced replace, a weird filesystem)
        // must not pass: the whole point is what a later open sees.
        if !std::fs::read(path).is_ok_and(|bytes| bytes.is_empty()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the path still reads its contents after the seal",
            ));
        }
        Ok(())
    }
}

/// Off Linux (macOS dev builds of the workspace) or off the supported arches there is nothing to
/// seal with, so a requested seal fails and agentd refuses to start, like `harden::apply_self`.
#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod imp {
    use std::{io, path::Path};

    pub fn seal_token_path(_path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sealing requires Linux on x86_64/aarch64",
        ))
    }
}

pub use imp::seal_token_path;
