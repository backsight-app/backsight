//! Game-only audio: process loopback of one process and its children, through
//! `ActivateAudioInterfaceAsync` (`VAD\Process_Loopback`). Same output and clock as desktop
//! loopback. Gotchas are in `docs/reference/platform-notes.md`.

use super::wasapi::{Device, Loopback, LoopbackStats};
use std::{
    mem::ManuallyDrop,
    sync::mpsc::{SyncSender, sync_channel},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{E_NOINTERFACE, ERROR_TIMEOUT},
        Media::Audio::*,
        System::{
            Com::{
                BLOB,
                StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0},
            },
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
            Variant::VT_BLOB,
        },
    },
    core::{Error, HRESULT, Interface, Ref, Result, implement, w},
};

/// First build with process loopback. Microsoft's reference says 20348, but it works on
/// Windows 10 2004 (19041) and later.
const MIN_BUILD: u32 = 19041;
/// How long Windows gets to finish the activation.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Captures what one process tree plays, on its own thread; dropping it stops the thread.
pub struct ProcessLoopback(Loopback);

impl ProcessLoopback {
    /// Captures what process `pid` and its child processes play, and calls `on_audio` exactly
    /// as [`Loopback::start`] does. Silence while they play nothing (or after they exit: the
    /// caller must watch the process itself).
    ///
    /// Fails only on Windows too old for it, with a message for the user. Other failures are
    /// retried every 500 ms with [`LoopbackStats::problem`] set, and the stream stays silent;
    /// it never falls back to whole-desktop sound.
    pub fn start(pid: u32, on_audio: impl FnMut(&[f32], u64) + Send + 'static) -> std::result::Result<Self, String> {
        if !supported(windows_build()) {
            return Err("Game-only audio needs Windows 10 version 2004 or newer. Update Windows, or record \
                        desktop audio instead."
                .into());
        }
        let explain = |_: &Error| "Game-only audio stopped working. Backsight keeps trying; record desktop audio if it doesn't come back.";
        Ok(ProcessLoopback(Loopback::spawn(
            "backsight-game-audio",
            None, // follows the process, not an endpoint
            move || open(pid),
            explain,
            on_audio,
        )))
    }

    pub fn stats(&self) -> LoopbackStats {
        self.0.stats()
    }

    /// Stops and joins the thread; [`ProcessLoopback::stats`] keeps the final counts afterwards.
    pub fn stop(&mut self) {
        self.0.stop();
    }
}

/// Unknown builds are tried: activation fails with its own error if it really is too old.
fn supported(build: Option<u32>) -> bool {
    build.is_none_or(|b| b >= MIN_BUILD)
}

/// The Windows build number. `GetVersionEx` reports Windows 8 to programs without a manifest
/// (tests), so read it where Windows keeps it.
fn windows_build() -> Option<u32> {
    let mut text = [0u16; 16];
    let mut bytes = size_of_val(&text) as u32;
    // SAFETY: `text` is writable for `bytes` bytes; both outlive the call.
    let e = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    e.ok().ok()?;
    String::from_utf16_lossy(&text[..bytes as usize / 2]).trim_end_matches('\0').parse().ok()
}

/// Signals when Windows has finished the activation. Objects made with `implement` are agile
/// (`IAgileObject` and the free-threaded marshaler), as the completion handler must be.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _: Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

fn open(pid: u32) -> Result<Device> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // A VT_BLOB pointing at `params`. `PROPVARIANT`'s `Drop` calls PropVariantClear, which
    // would free `params` as if Windows had allocated it (heap corruption), so never drop it.
    let blob = BLOB { cbSize: size_of_val(&params) as u32, pBlobData: (&raw mut params).cast() };
    let prop = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                Anonymous: PROPVARIANT_0_0_0 { blob },
                ..Default::default()
            }),
        },
    });
    let (done, finished) = sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(done).into();
    // SAFETY: `prop` and `params` outlive the wait for completion below; the handler is
    // reference counted, so Windows keeps it alive even if we stop waiting.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*prop), &handler)?
    };
    finished.recv_timeout(ACTIVATE_TIMEOUT).map_err(|_| Error::from(ERROR_TIMEOUT))?;
    let (mut result, mut client) = (HRESULT(0), None);
    // SAFETY: the operation has completed; both out-pointers are live.
    unsafe { operation.GetActivateResult(&mut result, &mut client)? };
    result.ok()?;
    let client: IAudioClient = client.ok_or_else(|| Error::from(E_NOINTERFACE))?.cast()?;
    // Process loopback has no GetMixFormat; Device::start asks for our format explicitly.
    Device::start(client, AUDCLNT_STREAMFLAGS_LOOPBACK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_windows_10_2004_unless_the_build_is_unknown() {
        assert!(!supported(Some(19000)));
        assert!(supported(Some(19041)));
        assert!(supported(Some(26100)));
        assert!(supported(None));
    }

    #[test]
    fn reads_the_windows_build() {
        assert!(windows_build().is_some_and(|b| b >= 10240), "{:?}", windows_build());
    }
}
