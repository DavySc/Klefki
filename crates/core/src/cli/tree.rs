//! Ends a process together with everything it started, such as the server that `claude mcp get` runs through npx
//! and node. On Unix the process leads a process group of its own; on Windows it is put in a job object.
//!
//! A timeout or a cancel ends a tree. When the process ends by itself, whatever it left running is left alone
//! (Claude may start something on purpose that outlives it, such as an update), unless the tree was started with
//! `end_leftovers`: such a tree is ended by its caller then too, and whenever Klefki itself ends while it runs.
//! On Unix that is when Klefki returns or exits, or is ended by SIGINT (Ctrl+C), SIGTERM or SIGHUP (its terminal
//! closed); SIGKILL cannot be caught. On Windows the job ends with Klefki however Klefki ends.
use std::io;
use std::process::{Child, Command};

/// Starts `cmd` so that it can be ended with everything it starts. On Windows it also gets no console window.
pub(crate) fn spawn(cmd: &mut Command, end_leftovers: bool) -> io::Result<(Child, Tree)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        if end_leftovers {
            live::watch();
        }
        // A signal that comes in before the group is listed waits until it is, or the child would outlive Klefki.
        let starting = end_leftovers.then(live::Starting::begin).transpose()?;
        let child = cmd.spawn()?;
        #[cfg(test)]
        if let Some(hook) = AFTER_SPAWN.get() {
            hook();
        }
        let mut tree = Tree::led_by(child.id());
        if end_leftovers && let Some(group) = tree.group {
            tree.listed = live::add(group);
        }
        drop(starting);
        Ok((child, tree))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Suspended until it is in the job, so nothing it starts can slip out of it.
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = cmd.spawn()?;
        let tree = Tree {
            job: sys::job_with(child.as_raw_handle(), end_leftovers),
        };
        if let Err(e) = sys::resume(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok((child, tree))
    }
}

/// Run by [`spawn`] on Unix once the process runs, before its group is listed: lets a test send a signal there.
#[cfg(all(test, unix))]
pub(crate) static AFTER_SPAWN: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// A started process and everything it starts.
pub(crate) struct Tree {
    #[cfg(unix)]
    group: Option<libc::pid_t>,
    /// The group is on the list of those ended when Klefki ends.
    #[cfg(unix)]
    listed: bool,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl Tree {
    /// The tree of a process started in a pseudo-terminal. On Unix it leads a session, and so a group, of its own.
    /// On Windows it can only be put in a job once it runs, so a process it started before that is not ended with
    /// it.
    pub(crate) fn of_pty(child: &dyn portable_pty::Child) -> Self {
        #[cfg(unix)]
        {
            child.process_id().map_or(
                Self {
                    group: None,
                    listed: false,
                },
                Self::led_by,
            )
        }
        #[cfg(windows)]
        {
            Self {
                job: child
                    .as_raw_handle()
                    .and_then(|process| sys::job_with(process, false)),
            }
        }
    }

    #[cfg(unix)]
    fn led_by(pid: u32) -> Self {
        Self {
            group: libc::pid_t::try_from(pid).ok().filter(|&p| p > 1),
            listed: false,
        }
    }

    /// Ends every process in the tree. Call it before the process itself has been waited for: on Unix its id names
    /// the group only until then. The caller still kills and waits for the process itself.
    pub(crate) fn kill(&self) {
        #[cfg(unix)]
        if let Some(group) = self.group {
            // SAFETY: a plain system call. The caller has not waited for the leader yet, so its id is still taken and
            // the group cannot be another one that reused it.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        #[cfg(unix)]
        self.unlist();
        #[cfg(windows)]
        if let Some(job) = &self.job {
            sys::terminate(job);
        }
    }

    /// Takes the group off the list before its leader is waited for, after which its id may be another's.
    #[cfg(unix)]
    fn unlist(&self) {
        if self.listed
            && let Some(group) = self.group
        {
            live::remove(group);
        }
    }
}

#[cfg(unix)]
impl Drop for Tree {
    fn drop(&mut self) {
        self.unlist();
    }
}

/// Has the process ended? Unlike [`Child::try_wait`] it is not waited for yet, so on Unix its id still names its
/// group: call [`Tree::kill`], then wait for it.
pub(crate) fn has_ended(child: &mut Child) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let pid: libc::id_t = child.id();
        loop {
            // SAFETY: all zeros is a valid `siginfo_t`; zeroed, it says "nothing ended" if the call leaves it alone.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a valid, writable `siginfo_t`. `WNOWAIT` leaves the process to be waited for.
            let r = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if r == 0 {
                // SAFETY: filled in by `waitid` for a process that changed state, all zeros otherwise.
                let changed = unsafe { info.si_pid() } != 0;
                // macOS also reports a stopped process here, despite `WEXITED` (golang/go#19314): only an end counts.
                let ended = matches!(
                    info.si_code,
                    libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
                );
                return Ok(changed && ended);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
    #[cfg(windows)]
    {
        // A job keeps its own handle: nothing can take its place, waited for or not.
        Ok(child.try_wait()?.is_some())
    }
}

/// The process groups to end when Klefki ends. A signal handler reads them, so they are plain atomics: no lock, no
/// allocation. A group that finds no free slot is still ended by its caller, only not when Klefki ends.
#[cfg(unix)]
mod live {
    use std::io;
    use std::sync::Once;
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

    const SLOTS: usize = 64;
    static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];
    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The starts in flight (the low bits) and a signal that came in during one (the bits above them). One word,
    /// so that the signal handler and the last start to finish cannot both leave the signal to the other.
    static STARTING: AtomicU32 = AtomicU32::new(0);
    const IN_FLIGHT: u32 = 0xFFFF;
    const SIGNAL_SHIFT: u32 = 16;

    /// A start whose group is not on the list yet. A signal that would end Klefki meanwhile is held until the last
    /// such start ends (dropped once its group is listed, or once it failed), and then acted on.
    pub(super) struct Starting(());

    impl Starting {
        /// Refused while a signal is held, so that new starts cannot keep putting off ending Klefki.
        pub(super) fn begin() -> io::Result<Self> {
            update(begun)
                .map(|_| Self(()))
                .map_err(|_| io::Error::from(io::ErrorKind::Interrupted))
        }
    }

    impl Drop for Starting {
        fn drop(&mut self) {
            if let Ok(before) = update(|word| Some(finished(word).0))
                && let (_, Some(signal)) = finished(before)
            {
                end_as(signal);
            }
        }
    }

    /// The word after a start begins, or `None` (refused) while a signal is held, or when the count is full.
    fn begun(word: u32) -> Option<u32> {
        (word >> SIGNAL_SHIFT == 0 && word & IN_FLIGHT < IN_FLIGHT).then(|| word + 1)
    }

    /// The word after `signal` came in, or `None` if no start is in flight and so the signal is acted on now. A
    /// signal already held gives way to the newer one.
    fn hold(word: u32, signal: libc::c_int) -> Option<u32> {
        (word & IN_FLIGHT != 0)
            .then(|| (word & IN_FLIGHT) | (signal.cast_unsigned() << SIGNAL_SHIFT))
    }

    /// The word after a start ends, and the signal it must act on: the held one, if it was the last start.
    fn finished(word: u32) -> (u32, Option<libc::c_int>) {
        let held = word >> SIGNAL_SHIFT;
        match word & IN_FLIGHT {
            1 => (0, (held != 0).then(|| held.cast_signed())),
            _ => (word.saturating_sub(1), None),
        }
    }

    /// Changes [`STARTING`] by `change` in one step, unless it returns `None`. Returns the word it changed, or the
    /// one it left alone. Only atomics, so a signal handler may call it.
    fn update(change: impl Fn(u32) -> Option<u32>) -> Result<u32, u32> {
        let mut word = STARTING.load(Ordering::SeqCst);
        loop {
            let after = change(word).ok_or(word)?;
            match STARTING.compare_exchange(word, after, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(before) => return Ok(before),
                Err(now) => word = now,
            }
        }
    }

    pub(super) fn add(group: libc::pid_t) -> bool {
        GROUPS.iter().any(|slot| {
            slot.compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        })
    }

    pub(super) fn remove(group: libc::pid_t) {
        for slot in &GROUPS {
            let _ = slot.compare_exchange(group, 0, Ordering::SeqCst, Ordering::SeqCst);
        }
    }

    /// Only calls that are safe in a signal handler.
    fn end_all() {
        for slot in &GROUPS {
            let group = slot.load(Ordering::SeqCst);
            if group > 1 {
                // SAFETY: a plain system call. A group on the list has a leader that was not waited for yet. Another
                // thread may take it off the list and wait for its leader just after this load; its id would then
                // have to be reused by a new group within that instant, while Klefki is ending.
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
            }
        }
    }

    extern "C" fn at_exit() {
        end_all();
    }

    extern "C" fn on_signal(signal: libc::c_int) {
        // While a group is being started, the start that finishes last acts on the signal instead.
        if update(|word| hold(word, signal)).is_err() {
            end_as(signal);
        }
    }

    /// Ends the listed groups, then Klefki as `signal` would have: the shell sees that Klefki was interrupted.
    /// Only calls that are safe in a signal handler.
    fn end_as(signal: libc::c_int) {
        end_all();
        // SAFETY: both calls are safe in a signal handler. From the handler, the signal stays blocked until it returns;
        // from a finishing start, it is delivered before `raise` returns.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }

    /// Once per process: ends the listed groups when it exits, and on the signals that would end it by default. A
    /// signal that is ignored (`nohup`) or already handled is left as it is.
    pub(super) fn watch() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // SAFETY: registers a function that takes no arguments and only makes system calls.
            unsafe {
                libc::atexit(at_exit);
            }
            for signal in SIGNALS {
                // SAFETY: all zeros is a valid `sigaction`; the first call only reads the current action into it.
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                        || old.sa_sigaction != libc::SIG_DFL
                    {
                        continue;
                    }
                    let mut new: libc::sigaction = std::mem::zeroed();
                    new.sa_sigaction =
                        on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    libc::sigemptyset(&mut new.sa_mask);
                    // A held signal returns from the handler: calls it interrupted go on.
                    new.sa_flags = libc::SA_RESTART;
                    libc::sigaction(signal, &new, std::ptr::null_mut());
                }
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const INT: libc::c_int = libc::SIGINT;
        const TERM: libc::c_int = libc::SIGTERM;

        #[test]
        fn only_the_last_start_to_finish_acts_on_a_held_signal() {
            let two = begun(begun(0).unwrap()).unwrap();
            let held = hold(two, INT).unwrap();
            // Regression variant: the first start to finish must leave the signal to the other, whose group is not
            // listed yet.
            let (one, act) = finished(held);
            assert_eq!(act, None);
            assert_eq!(one & IN_FLIGHT, 1);
            assert_eq!(finished(one), (0, Some(INT)));
        }

        #[test]
        fn a_signal_with_no_start_in_flight_is_acted_on_at_once() {
            assert_eq!(hold(0, INT), None);
            let (idle, act) = finished(begun(0).unwrap());
            assert_eq!((idle, act), (0, None));
            assert_eq!(hold(idle, TERM), None);
        }

        #[test]
        fn a_newer_signal_replaces_the_held_one() {
            let one = begun(0).unwrap();
            let held = hold(hold(one, INT).unwrap(), TERM).unwrap();
            assert_eq!(finished(held), (0, Some(TERM)));
        }

        #[test]
        fn no_start_begins_while_a_signal_is_held() {
            let held = hold(begun(0).unwrap(), INT).unwrap();
            assert_eq!(begun(held), None);
            assert_eq!(begun(IN_FLIGHT), None, "a full count is refused too");
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    /// A new job holding `process`, or `None` if the system refuses (then only the process itself is ended). With
    /// `end_leftovers`, the job ends when its last handle closes: Klefki's, however Klefki ends.
    pub(super) fn job_with(process: RawHandle, end_leftovers: bool) -> Option<OwnedHandle> {
        // SAFETY: no name and no security attributes; a handle returned is owned from here on.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return None;
        }
        // SAFETY: `job` was just created and nothing else owns it.
        let job = unsafe { OwnedHandle::from_raw_handle(job as RawHandle) };
        // A process that asks to leave the job on purpose may (it is then not ended with it); without this its start
        // would fail. If the system refuses, the job is used as it is.
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK
            | if end_leftovers {
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            } else {
                0
            };
        // SAFETY: the handle is open and `limits` is the structure this information class takes, with its size.
        unsafe {
            SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
        }
        // SAFETY: both handles are open for the length of the call.
        let ok =
            unsafe { AssignProcessToJobObject(job.as_raw_handle() as HANDLE, process as HANDLE) };
        (ok != 0).then_some(job)
    }

    pub(super) fn terminate(job: &OwnedHandle) {
        // SAFETY: the handle is open; ending the job's processes is what it is for.
        unsafe {
            TerminateJobObject(job.as_raw_handle() as HANDLE, 1);
        }
    }

    /// Lets a process started suspended run: resumes its threads (it has one).
    pub(super) fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: takes a snapshot of the system's threads; a handle returned is owned from here on.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `snapshot` was just created and nothing else owns it.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot as RawHandle) };
        let snapshot = snapshot.as_raw_handle() as HANDLE;
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut resumed = false;
        // SAFETY: `entry` is a valid THREADENTRY32 with its size set, as both calls require.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: opens the thread by id; a handle returned is owned from here on.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    // SAFETY: `thread` was just opened and nothing else owns it.
                    let thread = unsafe { OwnedHandle::from_raw_handle(thread as RawHandle) };
                    // SAFETY: the handle is open with the right to resume.
                    resumed |=
                        unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } != u32::MAX;
                }
            }
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other("the started process could not be resumed"))
        }
    }
}
