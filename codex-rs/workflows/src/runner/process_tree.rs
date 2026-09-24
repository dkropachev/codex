use std::io;
use std::process::Child;
use std::process::Command;

/// Owns the isolated process tree for one synchronous workflow operation.
///
/// Unix children start in a dedicated process group. Windows children start suspended, are
/// assigned to a kill-on-close Job Object, and are resumed only after assignment succeeds.
pub(super) struct WorkflowProcessTree {
    #[cfg(unix)]
    process_group_id: Option<rustix::process::Pid>,
    #[cfg(windows)]
    job: Option<WindowsJob>,
}

impl WorkflowProcessTree {
    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        configure_command(command);
        let mut child = command.spawn()?;
        match Self::from_child(&child).and_then(|process_tree| {
            resume_child(&child)?;
            Ok(process_tree)
        }) {
            Ok(process_tree) => Ok((child, process_tree)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    fn from_child(child: &Child) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                process_group_id: Some(rustix::process::Pid::from_child(child)),
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                job: Some(WindowsJob::create(child)?),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    pub(super) fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id.take() {
            let _ = rustix::process::kill_process_group(
                process_group_id,
                rustix::process::Signal::KILL,
            );
        }

        #[cfg(windows)]
        if let Some(mut job) = self.job.take() {
            job.terminate();
        }
    }
}

impl Drop for WorkflowProcessTree {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
fn configure_command(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(/*pgroup*/ 0);
    #[cfg(target_os = "linux")]
    {
        let parent_pid = rustix::process::getpid();
        // SAFETY: the hook only invokes async-signal-safe process syscalls before exec.
        unsafe {
            command.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::TERM,
                ))
                .map_err(io::Error::from)?;
                if rustix::process::getppid() != Some(parent_pid) {
                    rustix::process::kill_process(
                        rustix::process::getpid(),
                        rustix::process::Signal::TERM,
                    )
                    .map_err(io::Error::from)?;
                }
                Ok(())
            });
        }
    }
}

#[cfg(windows)]
fn configure_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    command.creation_flags(CREATE_SUSPENDED);
}

#[cfg(not(any(unix, windows)))]
fn configure_command(_command: &mut Command) {}

#[cfg(windows)]
fn resume_child(child: &Child) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        #[link_name = "NtResumeProcess"]
        fn nt_resume_process(process_handle: HANDLE) -> i32;
    }

    let status = unsafe { nt_resume_process(child.as_raw_handle() as HANDLE) };
    if status < 0 {
        Err(io::Error::other(format!(
            "failed to resume workflow process: NTSTATUS {status:#x}"
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn resume_child(_child: &Child) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
struct WindowsJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    fn create(child: &Child) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
        use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
        use windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
        use windows_sys::Win32::System::JobObjects::SetInformationJobObject;

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle == 0 {
            return Err(last_os_error("failed to create workflow process job"));
        }
        let job = Self { handle };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(last_os_error("failed to configure workflow process job"));
        }
        let assigned =
            unsafe { AssignProcessToJobObject(job.handle, child.as_raw_handle() as HANDLE) };
        if assigned == 0 {
            return Err(last_os_error(
                "failed to assign workflow process to isolated job",
            ));
        }
        Ok(job)
    }

    fn terminate(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if self.handle == 0 {
            return;
        }
        unsafe {
            TerminateJobObject(self.handle, /*uexitcode*/ 1);
            CloseHandle(self.handle);
        }
        self.handle = 0;
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(windows)]
fn last_os_error(context: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{context}: {error}"))
}
