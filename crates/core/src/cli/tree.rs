//! Ends a process together with everything it started, such as the server that `claude mcp get` runs through npx
//! and node. On Unix the process leads a process group of its own; on Windows it is put in a job object.
//!
//! Only a timeout or a cancel ends a tree. When the process ends by itself, whatever it left running is left alone:
//! Claude may start something on purpose that outlives it, such as an update.
use std::io;
use std::process::{Child, Command};

/// Starts `cmd` so that it can be ended with everything it starts. On Windows it also gets no console window.
pub(crate) fn spawn(cmd: &mut Command) -> io::Result<(Child, Tree)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let tree = Tree::led_by(child.id());
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
            job: sys::job_with(child.as_raw_handle()),
        };
        if let Err(e) = sys::resume(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok((child, tree))
    }
}

/// A started process and everything it starts.
pub(crate) struct Tree {
    #[cfg(unix)]
    group: Option<libc::pid_t>,
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
            child
                .process_id()
                .map_or(Self { group: None }, Self::led_by)
        }
        #[cfg(windows)]
        {
            Self {
                job: child.as_raw_handle().and_then(sys::job_with),
            }
        }
    }

    #[cfg(unix)]
    fn led_by(pid: u32) -> Self {
        Self {
            group: libc::pid_t::try_from(pid).ok().filter(|&p| p > 1),
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
        #[cfg(windows)]
        if let Some(job) = &self.job {
            sys::terminate(job);
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
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    /// A new job holding `process`, or `None` if the system refuses (then only the process itself is ended).
    pub(super) fn job_with(process: RawHandle) -> Option<OwnedHandle> {
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
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
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
