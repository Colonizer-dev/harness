//! The `colonizer` command. Everything lives in the library — `src/main.rs` is the library root on
//! purpose, so the repository-level checks in `crates/repo-contracts` can link it — and this binary
//! is only the entry point the operating system calls.

fn main() -> std::process::ExitCode {
    colonizer_harness::main()
}
