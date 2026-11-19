//! Priority of the real-time threads (ADR 0017).
//!
//! The render thread and the threads woken at the time of a tick or a block (audio, paced
//! inputs, replay players, the tick relay of the HTML pages, the headphones) call
//! [`raise_current_thread`] once, first thing. A refusal leaves the thread as it was: it is
//! never an error. What each role got is published by [`summary`] for the statistics and the
//! soak report.
//!
//! - Windows: `THREAD_PRIORITY_TIME_CRITICAL` (render) or `THREAD_PRIORITY_HIGHEST` (timing),
//!   no power throttling for the thread (no EcoQoS) and, once, for the process.
//! - Linux: `SCHED_FIFO` 12 (render) or 10 (timing), reset on fork, granted only with the
//!   right to real time (`RLIMIT_RTPRIO`).
//!
//! The only `unsafe` of the crate: the system calls, each with its `SAFETY` note.
#![allow(unsafe_code)]

use std::sync::Once;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// What a thread does, which sets the priority it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    /// The render thread (`vma-render`): the highest.
    Render,
    /// A thread woken at the time of a tick or a block (audio, paced inputs, replay…).
    Timing,
}

/// The priority a thread got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Raised: what the system granted (`"SCHED_FIFO 12"`, `"TIME_CRITICAL"`…).
    Raised(&'static str),
    /// Left at the default priority, with the reason (in French: shown as is).
    Normal(&'static str),
}

impl Priority {
    pub fn is_raised(self) -> bool {
        matches!(self, Self::Raised(_))
    }
}

/// What the threads got so far, for the statistics and the soak report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrioritySummary {
    /// The render thread (`None`: not started yet).
    pub render: Option<Priority>,
    /// Timing threads raised, and left at the default priority, since the start.
    pub timing_raised: u32,
    pub timing_normal: u32,
    /// What a timing thread gets when raised (the same for all of them).
    pub timing_level: &'static str,
    /// Why the first timing thread left at the default priority was (`None`: none was).
    pub timing_reason: Option<&'static str>,
    /// Windows: the power throttling of the process was lifted (`None`: not tried, or not
    /// Windows).
    pub process_unthrottled: Option<bool>,
}

impl PrioritySummary {
    /// The render thread's priority, as shown in the statistics and the soak report.
    pub fn render_text(&self) -> String {
        match self.render {
            Some(Priority::Raised(level)) => format!("élevée ({level})"),
            Some(Priority::Normal(reason)) => format!("normale : {reason}"),
            None => "—".into(),
        }
    }

    /// The timing threads' priority, as shown in the statistics and the soak report.
    pub fn timing_text(&self) -> String {
        let reason = self.timing_reason.unwrap_or(os::DEFAULT_REASON);
        match (self.timing_raised, self.timing_normal) {
            (0, 0) => "aucun thread".into(),
            (raised, 0) => format!("élevée ({}) : {raised} threads", self.timing_level),
            (0, normal) => format!("normale : {reason} ({normal} threads)"),
            (raised, normal) => format!(
                "{raised} élevés ({}), {normal} normaux : {reason}",
                self.timing_level
            ),
        }
    }

    /// The power throttling of the process (Windows), if it was tried.
    pub fn process_text(&self) -> Option<&'static str> {
        self.process_unthrottled
            .map(|lifted| if lifted { "levé" } else { "non levé" })
    }
}

const UNKNOWN: u8 = 0;
const RAISED: u8 = 1;
const NORMAL: u8 = 2;

static RENDER: AtomicU8 = AtomicU8::new(UNKNOWN);
static RENDER_REASON: OnceLock<&'static str> = OnceLock::new();
static TIMING_RAISED: AtomicU32 = AtomicU32::new(0);
static TIMING_NORMAL: AtomicU32 = AtomicU32::new(0);
static TIMING_REASON: OnceLock<&'static str> = OnceLock::new();
static PROCESS: AtomicU8 = AtomicU8::new(UNKNOWN);
static PROCESS_ONCE: Once = Once::new();

/// Raises the priority of the calling thread for its role; never fails (a refusal leaves it
/// at the default priority, logged once per role). The first call also lifts the power
/// throttling of the process (Windows).
pub fn raise_current_thread(role: ThreadRole) -> Priority {
    PROCESS_ONCE.call_once(|| {
        if let Some(lifted) = os::unthrottle_process() {
            PROCESS.store(if lifted { RAISED } else { NORMAL }, Ordering::Release);
            if !lifted {
                tracing::warn!("the power throttling of the process could not be lifted");
            }
        }
    });
    let priority = match os::raise(role) {
        Ok(level) => Priority::Raised(level),
        Err(reason) => Priority::Normal(reason),
    };
    let name = std::thread::current().name().map(str::to_owned);
    match (role, priority) {
        (ThreadRole::Render, Priority::Raised(level)) => {
            RENDER.store(RAISED, Ordering::Release);
            tracing::info!(thread = ?name, level, "render thread priority raised");
        }
        (ThreadRole::Render, Priority::Normal(reason)) => {
            let _ = RENDER_REASON.set(reason);
            RENDER.store(NORMAL, Ordering::Release);
            tracing::info!(thread = ?name, reason, "render thread left at the default priority");
        }
        (ThreadRole::Timing, Priority::Raised(level)) => {
            TIMING_RAISED.fetch_add(1, Ordering::AcqRel);
            tracing::debug!(thread = ?name, level, "timing thread priority raised");
        }
        (ThreadRole::Timing, Priority::Normal(reason)) => {
            TIMING_NORMAL.fetch_add(1, Ordering::AcqRel);
            if TIMING_REASON.set(reason).is_ok() {
                tracing::info!(thread = ?name, reason, "timing threads left at the default priority");
            }
        }
    }
    priority
}

/// What the threads got so far.
pub fn summary() -> PrioritySummary {
    let render = match RENDER.load(Ordering::Acquire) {
        RAISED => Some(Priority::Raised(os::level(ThreadRole::Render))),
        NORMAL => Some(Priority::Normal(
            RENDER_REASON.get().copied().unwrap_or(os::DEFAULT_REASON),
        )),
        _ => None,
    };
    PrioritySummary {
        render,
        timing_raised: TIMING_RAISED.load(Ordering::Acquire),
        timing_normal: TIMING_NORMAL.load(Ordering::Acquire),
        timing_level: os::level(ThreadRole::Timing),
        timing_reason: TIMING_REASON.get().copied(),
        process_unthrottled: match PROCESS.load(Ordering::Acquire) {
            RAISED => Some(true),
            NORMAL => Some(false),
            _ => None,
        },
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::ThreadRole;

    pub(super) const DEFAULT_REASON: &str = "refusée par le système";

    pub(super) fn level(role: ThreadRole) -> &'static str {
        match role {
            ThreadRole::Render => "SCHED_FIFO 12",
            ThreadRole::Timing => "SCHED_FIFO 10",
        }
    }

    fn fifo_priority(role: ThreadRole) -> libc::c_int {
        match role {
            ThreadRole::Render => 12,
            ThreadRole::Timing => 10,
        }
    }

    pub(super) fn raise(role: ThreadRole) -> Result<&'static str, &'static str> {
        let param = libc::sched_param {
            sched_priority: fifo_priority(role),
        };
        // SAFETY: `sched_setscheduler` only reads `param`, a live local for the whole call;
        // pid 0 is the calling thread (Linux schedules threads), which exists.
        let rc = unsafe {
            libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param)
        };
        if rc == 0 {
            return Ok(level(role));
        }
        Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => "pas de droit au temps réel (RLIMIT_RTPRIO, ADR 0017)",
            _ => DEFAULT_REASON,
        })
    }

    pub(super) fn unthrottle_process() -> Option<bool> {
        None
    }
}

#[cfg(windows)]
mod os {
    use super::ThreadRole;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
        SetThreadInformation, SetThreadPriority, THREAD_POWER_THROTTLING_CURRENT_VERSION,
        THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
        THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_TIME_CRITICAL, ThreadPowerThrottling,
    };

    pub(super) const DEFAULT_REASON: &str = "refusée par Windows";

    pub(super) fn level(role: ThreadRole) -> &'static str {
        match role {
            ThreadRole::Render => "TIME_CRITICAL",
            ThreadRole::Timing => "HIGHEST",
        }
    }

    pub(super) fn raise(role: ThreadRole) -> Result<&'static str, &'static str> {
        let priority = match role {
            ThreadRole::Render => THREAD_PRIORITY_TIME_CRITICAL,
            ThreadRole::Timing => THREAD_PRIORITY_HIGHEST,
        };
        // SAFETY: `GetCurrentThread` returns a pseudo-handle to the calling thread, valid for
        // as long as it runs and never to be closed; `SetThreadPriority` takes it by value.
        let ok = unsafe { SetThreadPriority(GetCurrentThread(), priority) };
        if ok == 0 {
            return Err(DEFAULT_REASON);
        }
        let state = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            // Control bit set, state bit clear: never throttled (no EcoQoS).
            StateMask: 0,
        };
        // SAFETY: the pointer is to `state`, a live local of exactly the size given, only read
        // during the call; the handle is the calling thread's pseudo-handle (see above).
        let unthrottled = unsafe {
            SetThreadInformation(
                GetCurrentThread(),
                ThreadPowerThrottling,
                std::ptr::from_ref(&state).cast(),
                size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if unthrottled == 0 {
            // Before Windows 10 1709: the priority alone.
            tracing::debug!("the power throttling of a thread could not be lifted");
        }
        Ok(level(role))
    }

    pub(super) fn unthrottle_process() -> Option<bool> {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            // Control bits set, state bits clear: full speed, timer resolution always kept.
            StateMask: 0,
        };
        // SAFETY: `GetCurrentProcess` returns the pseudo-handle of this process (never
        // closed); the pointer is to `state`, a live local of exactly the size given, only read
        // during the call.
        let ok = unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                std::ptr::from_ref(&state).cast(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        Some(ok != 0)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod os {
    use super::ThreadRole;

    pub(super) const DEFAULT_REASON: &str = "non prise en charge sur ce système";

    pub(super) fn level(_role: ThreadRole) -> &'static str {
        "—"
    }

    pub(super) fn raise(_role: ThreadRole) -> Result<&'static str, &'static str> {
        Err(DEFAULT_REASON)
    }

    pub(super) fn unthrottle_process() -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_a_thread(role: ThreadRole) -> Priority {
        std::thread::spawn(move || raise_current_thread(role))
            .join()
            .unwrap()
    }

    #[test]
    fn a_raised_or_refused_thread_is_counted() {
        let before = summary();
        let got = on_a_thread(ThreadRole::Timing);
        let after = summary();
        if got.is_raised() {
            // At least one more: other tests of the same process may raise threads meanwhile.
            assert!(after.timing_raised > before.timing_raised);
            assert_eq!(got, Priority::Raised(after.timing_level));
        } else {
            assert!(after.timing_normal > before.timing_normal);
            assert!(after.timing_reason.is_some());
        }
        let render = on_a_thread(ThreadRole::Render);
        assert_eq!(summary().render, Some(render));
        assert!(!summary().render_text().is_empty() && summary().timing_text() != "aucun thread");
    }

    /// Windows grants these priorities without any right: they must be raised.
    #[cfg(windows)]
    #[test]
    fn windows_raises_both_roles_and_the_process() {
        assert_eq!(
            on_a_thread(ThreadRole::Render),
            Priority::Raised("TIME_CRITICAL")
        );
        assert_eq!(on_a_thread(ThreadRole::Timing), Priority::Raised("HIGHEST"));
        assert_eq!(summary().process_unthrottled, Some(true));
    }

    /// Linux: the policy the thread ends with is the one reported, and a refusal changes
    /// nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reports_the_policy_the_thread_has() {
        let (got, policy) = std::thread::spawn(|| {
            let got = raise_current_thread(ThreadRole::Timing);
            // SAFETY: no pointer; pid 0 is the calling thread.
            let policy = unsafe { libc::sched_getscheduler(0) };
            (got, policy)
        })
        .join()
        .unwrap();
        let policy = policy & !libc::SCHED_RESET_ON_FORK;
        if got.is_raised() {
            assert_eq!(policy, libc::SCHED_FIFO);
        } else {
            assert_eq!(policy, libc::SCHED_OTHER);
        }
    }
}
