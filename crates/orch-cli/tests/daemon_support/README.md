# Daemon fixture cleanup

Normal exit and panic use the existing Rust child and tmux drop guards. On Linux,
each `Sandbox` and socket-backed skew `Scene` also arms a Python watchdog before
frontends launch. Python runs with `-I -S` and a cleared environment. This is test
support only; the product daemon retains its independent lifetime.

The test holds the sole write end of a close-on-exec pipe. EOF tells the watchdog
that its fixture was dropped or its test process died, including SIGKILL. A stopped
but live test keeps the pipe open. This avoids polling a parent PID that can be
reused or remain a zombie. Watchdog readiness checks pidfd support; unsupported
Linux/Python versions fail fixture setup rather than silently omitting cleanup.

After EOF, observation and signalling have a five-second budget. Each scan accepts
at most 100,000 `/proc` entries and 64 KiB of environment per candidate. A target
must have the exact `ORCH_TEST_SOCKET_ROOT` entry for this unique private directory
and the device/inode of this checkout's orch executable, the resolved tmux binary,
`/bin/sh`, or `/bin/sleep`. The private tmux launcher exports the marker too. A
pidfd pins the process; ownership is checked again before sending SIGKILL through
that descriptor. No name/argv match or discovered process group receives a signal.
Scans repeat after process exit to cover marked children forked during cleanup.
The watcher then removes its two recorded temporary directories only while their
device/inode identities still match, refusing a replacement root or symlink.

The bounds cover observation and signalling, not arbitrary filesystem stalls or
recursive deletion of an unbounded directory. Fixtures contain synthetic data on
local disk. This is cooperative fixture ownership, not an access-control boundary
against hostile same-user processes that forge markers. A process that erases its
marker, changes to an unlisted executable, or outlives the budget may escape; the
watchdog itself can be killed. The short allocation/readiness interval before the
watchdog is armed is also outside the backstop. macOS retains normal drop cleanup;
its interrupted-run verification is deferred. Normal child reaping remains with
the Rust fixture; an orphan's final zombie reaping belongs to the system reaper.

`orphans.rs::watchdog_cleans_after_sigkill_without_signalling_foreign_fixtures`
runs the synthetic Python controls in each integration suite importing these
helpers. Run the controls directly with an isolated HOME and TMPDIR:

```sh
python3 -I -S crates/orch-cli/tests/daemon_support/orphan_watchdog_test.py -v
cargo test -p orch-cli --test daemon_nodaemon_matrix
```

The controls kill only retained synthetic child handles, use pidfd readiness for
exit observation, and retain emergency cleanup if the watchdog is deliberately
broken. They cover SIGKILL, live siblings sharing the same executable, foreign
unmarked processes, incorrect executable identity, marker prefixes, and replaced
directories. Real TUI acceptance additionally kills the matrix test once its owner
daemon starts and checks the private tmux, wrappers, TUI, daemon and paths while a
live sibling and unrelated private tmux session survive.
