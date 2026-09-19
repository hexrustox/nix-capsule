//! Shell-script builders for child processes: trap shapes and group
//! layouts whose observable effect is a marker file or stdout line. Tests
//! state which script they need; the quoting and trap layout stays here.

use std::path::Path;

/// Tick interval inside child scripts: feed loops that stream to a client
/// and heartbeat stamps that witness a full TERM grace.
pub(crate) const SHELL_TICK: f64 = 0.2;
/// Bounded hold inside child scripts: the red-run bound, the trap aftermath,
/// and the signal-loop tick.
pub(crate) const SHELL_HOLD: u32 = 1;
/// Survivor body: a child sleep that must outlive every test bound.
pub(crate) const SHELL_BODY: u32 = 30;

/// The script for a child that announces READY, ticks every 200 ms, and
/// announces its own death in `marker` from the TERM trap the disconnect
/// fires, then exits.
pub(crate) fn trapping_ticker_script(marker: &Path) -> String {
    format!(
        "trap 'echo gone >> {}; exit 0' TERM; echo READY; \
         while true; do echo tick; sleep {SHELL_TICK}; done",
        marker.display()
    )
}

/// Both shells announce their own death from a TERM trap; the grandchild's
/// line can only come from a TERM it received itself — a kill of the child
/// alone would leave the grandchild to die silently of SIGPIPE (disconnect)
/// or to keep running (shutdown).
pub(crate) fn group_trap_script(marker: &Path) -> String {
    format!(
        "trap 'echo child-gone >> {}; exit 0' TERM; \
         ( trap 'echo grandchild-gone >> {}; exit 0' TERM; \
           while true; do echo tick-gc; sleep {SHELL_TICK}; done ) & \
         echo READY; wait",
        marker.display(),
        marker.display()
    )
}

/// The trap runs on TERM but does not exit, and silences stdout
/// (`exec 1>/dev/null`) so the remaining ticks cannot SIGPIPE the child
/// away once the server drops the pipe. The heartbeats it then stamps into
/// `marker` are the no-escalation witness: a server that KILLed after the
/// TERM would stop them early.
pub(crate) fn surviving_trap_script(marker: &Path) -> String {
    format!(
        "trap 'echo trapped >> {}; exec 1>/dev/null' TERM; echo A-READY; \
         for i in 1 2 3 4 5 6 7 8 9 10; do echo tick-a; sleep {SHELL_TICK}; done; \
         for i in 1 2 3 4 5; do sleep {SHELL_TICK}; echo alive-$i >> {}; done",
        marker.display(),
        marker.display()
    )
}

/// Shutdown variant of the group trap (ticks `tick` instead of `tick-gc`).
pub(crate) fn shutdown_group_trap_script(marker: &Path) -> String {
    format!(
        "trap 'echo child-gone >> {}; exit 0' TERM; \
         ( trap 'echo grandchild-gone >> {}; exit 0' TERM; \
           while true; do echo tick; sleep {SHELL_TICK}; done ) & \
         echo READY; wait",
        marker.display(),
        marker.display()
    )
}
