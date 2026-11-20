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
/// (`IAgileObject` and the free-threaded marshaler), as the completion handler must be. It also
/// owns the activation parameters: Microsoft doesn't say Windows copies them, and Windows keeps
/// the handler alive until it has called it, even after we stop waiting.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(SyncSender<()>, #[allow(dead_code)] Owned);

/// Owns the `Params` allocation through a raw pointer, so the pointers into it handed to Windows
/// stay valid however this wrapper moves (a moved `Box` would invalidate them under Stacked Borrows).
struct Owned(*mut Params);

// SAFETY: the pointee is plain data that only Windows reads; ours only frees it, from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above; no shared mutation from our side.
unsafe impl Sync for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: made by Box::into_raw in `open` and freed only here, once.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

/// The process-loopback parameters and the VT_BLOB `PROPVARIANT` pointing at them, at a fixed
/// heap address. `PROPVARIANT`'s `Drop` calls PropVariantClear, which would free `params` as if
/// Windows had allocated it (heap corruption), so `prop` is never dropped.
struct Params {
    params: AUDIOCLIENT_ACTIVATION_PARAMS,
    prop: ManuallyDrop<PROPVARIANT>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _: Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

fn open(pid: u32) -> Result<Device> {
    let raw = Box::into_raw(Box::new(Params {
        params: AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        },
        prop: ManuallyDrop::new(PROPVARIANT::default()),
    }));
    let owned = Owned(raw);
    // SAFETY: `raw` is a live, uniquely owned allocation; these place projections make no references.
    let blob = BLOB {
        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        pBlobData: unsafe { &raw mut (*raw).params }.cast(),
    };
    // SAFETY: as above; writing a field that holds no resources (ManuallyDrop of a default value).
    unsafe {
        (*raw).prop = ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    Anonymous: PROPVARIANT_0_0_0 { blob },
                    ..Default::default()
                }),
            },
        })
    };
    // SAFETY: as above; ManuallyDrop is repr(transparent), so the cast keeps the layout.
    let prop: *const PROPVARIANT = unsafe { &raw const (*raw).prop }.cast();
    let (done, finished) = sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(done, owned).into();
    // SAFETY: `prop` and the parameters it points at live in the handler's box, which Windows keeps
    // (with the handler) until the activation completes, even if the wait below times out.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(prop), &handler)?
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
