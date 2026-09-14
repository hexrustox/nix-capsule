//! Shell-script builders for child processes: trap shapes and group
//! layouts whose observable effect is a marker file or stdout line. Tests
//! state which script they need; the quoting and trap layout stays here.

use std::path::Path;

/// The script for a child that announces READY, ticks every 300 ms, and
/// announces its own death in `marker` from the TERM trap the disconnect
/// fires, then exits.
pub fn trapping_ticker_script(marker: &Path) -> String {
    format!(
        "trap 'echo gone >> {}; exit 0' TERM; echo READY; \
         while true; do echo tick; sleep 0.3; done",
        marker.display()
    )
}

/// Both shells announce their own death from a TERM trap; the grandchild's
/// line can only come from a TERM it received itself — a kill of the child
/// alone would leave the grandchild to die silently of SIGPIPE (disconnect)
/// or to keep running (shutdown).
pub fn group_trap_script(marker: &Path) -> String {
    format!(
        "trap 'echo child-gone >> {}; exit 0' TERM; \
         ( trap 'echo grandchild-gone >> {}; exit 0' TERM; \
           while true; do echo tick-gc; sleep 0.3; done ) & \
         echo READY; wait",
        marker.display(),
        marker.display()
    )
}

/// Shutdown variant of the group trap (ticks `tick` instead of `tick-gc`).
pub fn shutdown_group_trap_script(marker: &Path) -> String {
    format!(
        "trap 'echo child-gone >> {}; exit 0' TERM; \
         ( trap 'echo grandchild-gone >> {}; exit 0' TERM; \
           while true; do echo tick; sleep 0.3; done ) & \
         echo READY; wait",
        marker.display(),
        marker.display()
    )
}
