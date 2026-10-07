#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::os::fd::IntoRawFd;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;

#[cfg(unix)]
use anyhow::Context;
#[cfg(unix)]
use anyhow::bail;

#[cfg(unix)]
pub(super) fn list_names(
    directory: &OwnedFd,
    maximum_entries: usize,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<Vec<String>> {
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::openat;

    let opened = openat(
        directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .context("failed to reopen managed workflow directory for enumeration")?;
    let raw = opened.into_raw_fd();
    // SAFETY: `raw` is a newly owned directory descriptor. `fdopendir` takes ownership on success.
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed and did not consume `raw`.
        unsafe { libc::close(raw) };
        return Err(error).context("failed to enumerate managed workflow directory");
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
            bail!("managed workflow directory scan was cancelled");
        }
        set_errno_zero()?;
        // SAFETY: the stream remains valid until this loop ends; the returned entry is used before
        // the next `readdir` call and copied into an owned `String`.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(0) {
                break;
            }
            return Err(error).context("failed to read managed workflow directory entry");
        }
        // SAFETY: POSIX guarantees a NUL-terminated `d_name` for a valid `dirent`.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(name.to_bytes())
            .context("managed workflow directory entry must be UTF-8")?;
        super::validate_component(name)?;
        names.push(name.to_owned());
        if names.len() > maximum_entries {
            bail!("managed workflow directory scan exceeds its entry limit");
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
struct DirectoryStream(*mut libc::DIR);

#[cfg(unix)]
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this stream is owned by the guard and closed exactly once.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_errno_zero() -> anyhow::Result<()> {
    // SAFETY: errno is thread-local and the pointer is valid for this thread.
    unsafe { *libc::__errno_location() = 0 };
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_errno_zero() -> anyhow::Result<()> {
    // SAFETY: errno is thread-local and the pointer is valid for this thread.
    unsafe { *libc::__error() = 0 };
    Ok(())
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
fn set_errno_zero() -> anyhow::Result<()> {
    bail!("managed workflow directory enumeration is unsupported on this Unix platform")
}
