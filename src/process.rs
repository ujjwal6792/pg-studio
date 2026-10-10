//! Safe process termination behind a single seam.
//!
//! Every place that stops a child process (session stop/fail, stale-entry
//! cleanup on startup, `pg_dump`/`pg_restore` cancel) funnels through here,
//! so signal ordering, PID-reuse tolerance, and reaping live in one auditable
//! module instead of scattered `unsafe` blocks at each call site.
//!
//! Policy: SIGTERM first, wait up to a grace deadline, then escalate to
//! SIGKILL. Direct-PID signalling only — pg-studio never places children in
//! their own process group, so negative-PGID group kills could only ever hit
//! an unrelated reused PID, never the child.

use std::process::Child;
use std::time::{Duration, Instant};

/// How long `terminate_*` waits after SIGTERM before escalating to SIGKILL.
pub const TERM_GRACE: Duration = Duration::from_secs(3);

/// Poll interval while waiting for a process to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How a termination ended. Labels are best-effort: a process that exits
/// exactly as the grace deadline passes may be reported either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateOutcome {
    /// The process had already exited, or exited gracefully; reaped.
    Exited,
    /// Grace expired without exit; escalated to SIGKILL (or `Child::kill`).
    Killed,
    /// No such process (ESRCH on the first signal); nothing was sent.
    AlreadyGone,
    /// PID signalling is unavailable on this platform; nothing was sent.
    Unsupported,
}

/// Best-effort signal delivery. Returns `true` when the process exists
/// (delivered, or EPERM-denied but alive), `false` when it is gone (ESRCH)
/// or the pid cannot be addressed on this platform.
#[cfg(unix)]
fn signal(pid: u32, sig: libc::c_int) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        // Unaddressable pid: never signal (a truncation would hit the
        // wrong process). Callers treat this as "gone".
        return false;
    };
    // SAFETY: `kill` with a positive pid and a valid signal number carries
    // no memory-safety obligations; failure surfaces via errno below.
    if unsafe { libc::kill(pid, sig) } == 0 {
        return true;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(errno) if errno == libc::ESRCH => false,
        // EPERM and friends mean the process exists; treat as alive.
        _ => true,
    }
}

/// Non-signalling liveness probe (`kill(pid, 0)`).
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    signal(pid, 0)
}

/// Terminates a process known only by pid (stale restored sessions,
/// `pg_dump` cancel where the `Child` handle lives elsewhere).
///
/// Cannot reap: the caller (or init) owns that. Prefer [`terminate_child`]
/// whenever the `Child` handle is available.
pub fn terminate_pid(pid: u32, grace: Duration) -> TerminateOutcome {
    #[cfg(not(unix))]
    {
        let _ = (pid, grace);
        return TerminateOutcome::Unsupported;
    }
    #[cfg(unix)]
    {
        if !signal(pid, libc::SIGTERM) {
            return TerminateOutcome::AlreadyGone;
        }
        let start = Instant::now();
        while start.elapsed() < grace {
            if !process_alive(pid) {
                return TerminateOutcome::Exited;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        if signal(pid, libc::SIGKILL) {
            TerminateOutcome::Killed
        } else {
            TerminateOutcome::Exited
        }
    }
}

/// Terminates a child owned through its `Child` handle.
///
/// Sends SIGTERM to `pid` when given (skipped on ESRCH — the child exited
/// between the first poll and the signal), waits up to `grace` via
/// `try_wait`, then escalates to [`Child::kill`] and reaps, so no zombie
/// is left behind. When `pid` is `None` the wait/kill still applies.
pub fn terminate_child(child: &mut Child, pid: Option<u32>, grace: Duration) -> TerminateOutcome {
    // Fast path: already exited, just reap. No signals needed.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return TerminateOutcome::Exited;
    }
    #[cfg(unix)]
    if let Some(pid) = pid {
        // NOTE: this SIGTERM can land before the child installs its own
        // handlers (startup race). That only kills faster — never slower —
        // so no synchronisation is needed here.
        signal(pid, libc::SIGTERM);
    }
    let start = Instant::now();
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return TerminateOutcome::Exited;
        }
        if start.elapsed() >= grace {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    let _ = child.kill();
    let _ = child.wait();
    TerminateOutcome::Killed
}

/// Intentionally leaks a tunnel handle so its child outlives pg-studio.
///
/// Used only by shutdown-detach: the SSH child is reparented and kept alive,
/// its pid persisted, and a later startup re-attaches or terminates it by
/// pid. The leak is the mechanism — call this instead of scattering
/// `mem::forget` at call sites so every intentional leak is visible here.
pub fn disown<T>(handle: Option<T>) {
    std::mem::forget(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const FAST_GRACE: Duration = Duration::from_millis(300);

    #[test]
    #[cfg(unix)]
    fn unknown_pid_is_already_gone() {
        // u32::MAX also exercises the pid_t conversion guard: it must
        // never be truncated into a signallable pid.
        assert_eq!(
            terminate_pid(u32::MAX, FAST_GRACE),
            TerminateOutcome::AlreadyGone
        );
    }

    #[test]
    #[cfg(unix)]
    fn exited_child_reports_exited_without_signals() {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let _ = child.wait();
        assert_eq!(
            terminate_child(&mut child, None, FAST_GRACE),
            TerminateOutcome::Exited
        );
    }

    #[test]
    #[cfg(unix)]
    fn cooperative_child_exits_gracefully_on_term() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let outcome = terminate_child(&mut child, Some(pid), Duration::from_secs(5));
        assert_eq!(outcome, TerminateOutcome::Exited);
        assert!(!process_alive(pid));
    }

    #[test]
    #[cfg(unix)]
    fn term_ignoring_child_is_killed_after_grace() {
        use std::io::BufRead;
        let mut child = Command::new("sh")
            .arg("-c")
            // `echo ready` synchronises with trap installation: without it,
            // SIGTERM can land before the shell processes `trap` and kill
            // it with the default disposition (a startup race, not a bug —
            // early TERM only kills faster in production).
            .arg("trap '' TERM; echo ready; exec sleep 30")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().expect("piped stdout"))
            .read_line(&mut ready)
            .expect("read ready");
        assert_eq!(ready.trim(), "ready");
        let outcome = terminate_child(&mut child, Some(pid), FAST_GRACE);
        assert_eq!(outcome, TerminateOutcome::Killed);
        assert!(!process_alive(pid));
    }

    #[test]
    #[cfg(unix)]
    fn pid_only_termination_kills() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let outcome = terminate_pid(pid, Duration::from_secs(5));
        assert!(matches!(
            outcome,
            TerminateOutcome::Exited | TerminateOutcome::Killed
        ));
        // Reap first: an unreaped zombie still answers kill(pid, 0).
        let _ = child.wait();
        assert!(!process_alive(pid));
    }

    #[test]
    fn disowning_nothing_is_safe() {
        disown::<Child>(None);
    }
}
