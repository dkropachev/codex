#[cfg(any(unix, windows))]
use std::cell::Cell;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

use crate::tasks::workflow_command::WORKFLOW_ERROR_MAX_BYTES;

pub(super) struct BoundedLine {
    pub(super) bytes: Vec<u8>,
    pub(super) oversized: bool,
}

pub(super) struct WorkflowControlReader {
    reader: BufReader<Box<dyn AsyncRead + Send + Unpin>>,
}

impl WorkflowControlReader {
    pub(super) fn new(reader: impl AsyncRead + Send + Unpin + 'static) -> Self {
        Self {
            reader: BufReader::new(Box::new(reader)),
        }
    }

    pub(super) async fn next_line(&mut self, max_bytes: usize) -> io::Result<BoundedLine> {
        let mut bytes = Vec::new();
        let mut content_len = 0_usize;
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
                continue;
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            let content = newline.map_or(&available[..consumed], |index| &available[..index]);
            content_len = content_len.saturating_add(content.len());
            let remaining = max_bytes.saturating_sub(bytes.len());
            bytes.extend_from_slice(&content[..content.len().min(remaining)]);
            self.reader.consume(consumed);
            if newline.is_some() {
                break;
            }
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
            content_len = content_len.saturating_sub(1);
        }
        Ok(BoundedLine {
            bytes,
            oversized: content_len > max_bytes,
        })
    }
}

pub(super) struct BoundedDiagnostics {
    retained: Arc<std::sync::Mutex<Vec<u8>>>,
    task: tokio_util::task::AbortOnDropHandle<()>,
}

impl BoundedDiagnostics {
    pub(super) fn new(mut reader: impl AsyncRead + Send + Unpin + 'static) -> Self {
        let retained = Arc::new(std::sync::Mutex::new(Vec::new()));
        let task_retained = Arc::clone(&retained);
        let task = tokio::spawn(async move {
            let mut buffer = [0_u8; 8 * 1024];
            while let Ok(read) = reader.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                let Ok(mut retained) = task_retained.lock() else {
                    break;
                };
                let remaining = (WORKFLOW_ERROR_MAX_BYTES + 1).saturating_sub(retained.len());
                retained.extend_from_slice(&buffer[..read.min(remaining)]);
            }
        });
        Self {
            retained,
            task: tokio_util::task::AbortOnDropHandle::new(task),
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        let _ = &self.task;
        self.retained
            .lock()
            .map(|retained| retained.clone())
            .unwrap_or_default()
    }

    pub(super) async fn finish(mut self) -> Vec<u8> {
        let _ = tokio::time::timeout(Duration::from_millis(/*millis*/ 250), &mut self.task).await;
        self.snapshot()
    }
}

pub(super) struct WorkflowProcessGroupGuard {
    #[cfg(unix)]
    process_group_id: Cell<Option<u32>>,
    #[cfg(windows)]
    job_handle: Cell<windows_sys::Win32::Foundation::HANDLE>,
}

impl WorkflowProcessGroupGuard {
    pub(super) fn new(child: &tokio::process::Child) -> Result<Self, String> {
        #[cfg(unix)]
        {
            Ok(Self {
                process_group_id: Cell::new(child.id()),
            })
        }
        #[cfg(windows)]
        {
            create_windows_job(child).map(|job_handle| Self {
                job_handle: Cell::new(job_handle),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    pub(super) fn terminate(&self) {
        #[cfg(unix)]
        {
            let Some(process_group_id) = self.process_group_id.take() else {
                return;
            };
            let _ = codex_utils_pty::process_group::kill_process_group(process_group_id);
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;

            let job_handle = self.job_handle.replace(/*val*/ 0);
            if job_handle != 0 {
                unsafe {
                    CloseHandle(job_handle);
                }
            }
        }
    }
}

#[cfg(windows)]
fn create_windows_job(
    child: &tokio::process::Child,
) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
    use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
    use windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
    use windows_sys::Win32::System::JobObjects::SetInformationJobObject;

    let job_handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if job_handle == 0 {
        return Err("failed to create workflow process job".to_string());
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job_handle,
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    let process_handle = child
        .raw_handle()
        .ok_or_else(|| "workflow process handle was unavailable".to_string())?
        as windows_sys::Win32::Foundation::HANDLE;
    let assigned =
        configured != 0 && unsafe { AssignProcessToJobObject(job_handle, process_handle) } != 0;
    if !assigned {
        unsafe {
            CloseHandle(job_handle);
        }
        return Err("failed to assign workflow process to job".to_string());
    }
    Ok(job_handle)
}

impl Drop for WorkflowProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(windows)]
pub(super) fn suspend_windows_process(command: &mut Command) {
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    command.creation_flags(CREATE_SUSPENDED);
}

#[cfg(not(windows))]
pub(super) fn suspend_windows_process(_command: &mut Command) {}

#[cfg(windows)]
pub(super) fn resume_windows_process(child: &tokio::process::Child) -> Result<(), String> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        #[link_name = "NtResumeProcess"]
        fn nt_resume_process(process_handle: windows_sys::Win32::Foundation::HANDLE) -> i32;
    }

    let process_handle = child
        .raw_handle()
        .ok_or_else(|| "workflow process handle was unavailable".to_string())?
        as windows_sys::Win32::Foundation::HANDLE;
    let status = unsafe { nt_resume_process(process_handle) };
    if status < 0 {
        Err(format!(
            "failed to resume workflow process with NTSTATUS {status:#x}"
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub(super) fn resume_windows_process(_child: &tokio::process::Child) -> Result<(), String> {
    Ok(())
}
