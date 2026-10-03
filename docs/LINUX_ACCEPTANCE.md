# Frozen Linux build, launcher, and native PTY acceptance

The final source was verified directly on 2026-10-03 after the imported-custom-
endpoint fix, using actual Linux/Unix PTYs under Ubuntu WSL. Commands started outside the checkout, in
`/mnt/c/Users/Arda/AppData/Local/Temp/opencode`. The native Linux toolchain and
build target were isolated from the Windows release target.

## Platform and environment

- Kernel: `Linux 6.18.33.2-microsoft-standard-WSL2`.
- Rust: `rustc 1.99.0 (b940084d7 2026-09-28)`.
- Cargo: `cargo 1.99.0 (5f94df478 2026-08-27)`.

The exact environment used was:

```sh
export RUSTUP_HOME=/mnt/c/Users/Arda/AppData/Local/Temp/opencode/rustup-linux-agent06
export CARGO_HOME=/mnt/c/Users/Arda/AppData/Local/Temp/opencode/cargo-linux-agent06
export CARGO_TARGET_DIR=/mnt/c/Users/Arda/AppData/Local/Temp/opencode/openraid-linux-agent06
export PATH=/mnt/c/Users/Arda/AppData/Local/Temp/opencode/cargo-linux-agent06/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
```

These are verification-host paths, not required application defaults. On another
Linux host, use its native Rust installation and corresponding checkout,
workspace, database, and optional target-directory paths.

## Build and outside-checkout launcher checks

```sh
bash /mnt/c/Users/Arda/Desktop/projeler/openraid/build_linux.sh
bash /mnt/c/Users/Arda/Desktop/projeler/openraid/run.sh --help
bash /mnt/c/Users/Arda/Desktop/projeler/openraid/run.sh demo \
  --agents 500 --no-tui --grace-secs 0 \
  --workspace /mnt/c/Users/Arda/AppData/Local/Temp/opencode \
  --database /mnt/c/Users/Arda/AppData/Local/Temp/opencode/direct-final-linux-500.sqlite3 \
  'Verify directly completed Linux global board and graceful drain'
```

All three checks exited **0**. The build used `--release --locked`, took
**1m14s** for the final rebuild, and reported the correct custom-target artifact:

```text
/mnt/c/Users/Arda/AppData/Local/Temp/opencode/openraid-linux-agent06/release/openraid
```

Its SHA-256 was:

```text
ef38ab2153d97ad30978d86f7f67edf59f5e79608e2affdee3701fa023907644
```

The help command forwarded arguments and displayed the public commands. The
offline 500-worker demo produced:

```json
{
  "agents": 500,
  "finished_agents": 500,
  "votes": 500,
  "board_messages": 1003,
  "elapsed_ms": 40186
}
```

All 500 workers drained and the optimized process exited normally. The SQLite
database was on mounted NTFS; elapsed time is specific to this platform/storage
configuration and is not a live-provider throughput measurement. Zero grace
controls consensus stability, not operation or request duration.

## Actual Unix native PTY suite

From the same external working directory:

```sh
cargo test --locked \
  --manifest-path /mnt/c/Users/Arda/Desktop/projeler/openraid/Cargo.toml \
  --test native_pty -- --nocapture
```

The frozen suite exited **0**, with **9 passed, 0 failed, 0 ignored**, and no
warnings. Rebuild time was **26.86s**; test execution was **0.42s**.

Coverage uses the real OS backend and includes:

- Native terminal handles, interaction, resize, and natural exit.
- Immediate large input without a startup deadlock.
- Full persisted output exceeding 96 KiB with bounded, exact byte pagination.
- Split Unicode codepoints recoverable through `content_hex`.
- Shared registry and fresh-global-board enforcement before mutations.
- Admission feedback when persistent PTYs occupy all process slots, while
  ordinary commands retain normal queueing with partial PTY occupancy.
- Explicit kill releasing a maximum-size concurrent write to a never-reading
  child.
- Explicit cleanup and last-handle shutdown reaping nested children.
- Unix cleanup of a background descendant retaining the slave after its shell
  leader already exited.

Production requests and operations have no duration-based aborts. Test deadlines
only diagnose a failed regression. No application, test child, or browser remained
running after these checks. Complementary frozen Windows acceptance includes
**8 ConPTY cases** inside the **208-test Rust gate**; final cross-platform
checklist and operator records are maintained in [VERIFICATION.md](VERIFICATION.md).
