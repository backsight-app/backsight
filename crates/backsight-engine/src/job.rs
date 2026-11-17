//! A Windows job object that kills the encoder-test processes when Backsight exits, however it
//! exits. Platform edge: the only place that calls the job-object API.

use std::{ffi::c_void, os::windows::io::AsRawHandle, process::Child, sync::OnceLock};
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        },
    },
    core::PCWSTR,
};

struct Job(HANDLE);

// SAFETY: a job handle is a reference to a kernel object; any thread may use it.
unsafe impl Send for Job {}
// SAFETY: as above; nothing here mutates the handle.
unsafe impl Sync for Job {}

/// The one job, made on first use and never closed: the OS closes it when Backsight exits, which
/// (with `KILL_ON_JOB_CLOSE`) ends every process in it. `None` if it couldn't be made.
static JOB: OnceLock<Option<Job>> = OnceLock::new();

fn create() -> Option<Job> {
    // SAFETY: a new unnamed job with default security; the handle is ours and is kept for the
    // life of the process. `info` is a plain struct that lives through the call, and the size
    // passed is its own.
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast::<c_void>(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(Job(job))
    }
}

/// Puts `child` in the job, so it dies with Backsight. Returns whether that worked; a child that
/// isn't in the job only costs a stray process when Backsight is killed while it runs.
pub fn kill_on_exit(child: &Child) -> bool {
    let Some(job) = JOB.get_or_init(create) else {
        tracing::warn!("couldn't make the job that ends test processes with Backsight");
        return false;
    };
    // SAFETY: both handles are valid: the job lives for the whole process and `child` owns its
    // process handle for the duration of the call.
    let result = unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())) };
    if let Err(e) = &result {
        tracing::warn!("couldn't put a test process in the job: {e}");
    }
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_child_joins_the_job() {
        let mut child = Command::new("ping").args(["-n", "30", "127.0.0.1"]).spawn().unwrap();
        let joined = kill_on_exit(&child);
        let _ = child.kill();
        let _ = child.wait();
        assert!(joined);
    }
}
