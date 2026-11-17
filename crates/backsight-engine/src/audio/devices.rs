//! Audio endpoints: the microphone list for Settings, and following device changes while a
//! source captures (`IMMNotificationClient`). Gotchas are in `docs/reference/platform-notes.md`.

use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::PROPERTYKEY,
        Media::Audio::*,
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            STGM_READ,
        },
    },
    core::{HSTRING, PCWSTR, Result, implement},
};

/// One microphone for the Settings list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MicDevice {
    /// The endpoint id, which settings' `audio.microphone_device` stores (`"default"` is the
    /// Windows default, which the UI offers as its own first choice). Opaque and stable across reboots.
    pub id: String,
    /// The name Windows Sound settings shows, e.g. "Microphone (USB Audio Device)".
    pub name: String,
}

/// The microphones that are plugged in and enabled, in Windows' order. Callable from any thread.
pub fn microphones() -> std::result::Result<Vec<MicDevice>, String> {
    // SAFETY: joins (or starts) COM on this thread; an STA thread answers RPC_E_CHANGED_MODE,
    // which is fine for these calls and must not be balanced with CoUninitialize.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let list = list(eCapture);
    if com.is_ok() {
        // SAFETY: balances the successful CoInitializeEx above; `list` dropped its COM objects.
        unsafe { CoUninitialize() };
    }
    list.map_err(|e| {
        tracing::warn!("listing microphones failed: {e} ({:#010x})", e.code().0);
        "Windows didn't list the microphones. Open the list again; if it stays empty, restart Windows.".into()
    })
}

fn list(flow: EDataFlow) -> Result<Vec<MicDevice>> {
    // SAFETY: COM calls on live objects; the id string is copied before CoTaskMemFree releases it.
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        (0..devices.GetCount()?)
            .map(|i| {
                let device = devices.Item(i)?;
                let raw = device.GetId()?;
                let id = String::from_utf16_lossy(raw.as_wide());
                CoTaskMemFree(Some(raw.0 as _));
                let name = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?.to_string();
                Ok(MicDevice { id, name })
            })
            .collect()
    }
}

/// The endpoint a capture thread records: the Windows default for `flow` when `device` is
/// empty, otherwise that endpoint id.
pub(super) struct Follow {
    pub flow: EDataFlow,
    pub device: HSTRING,
}

/// A device event that may move the stream.
#[derive(Clone, Copy)]
enum Event<'a> {
    DefaultChanged(EDataFlow, ERole),
    /// The endpoint with this id became active (plugged back in, or enabled).
    Activated(&'a [u16]),
}

/// Whether `event` means the source should reopen now. A stream on the default follows the
/// console default (what `GetDefaultAudioEndpoint(.., eConsole)` opened); a chosen device
/// reopens as soon as it is back. Losing a device needs no event: the next WASAPI call fails
/// and the thread retries.
fn wants_reopen(follow: &Follow, event: Event) -> bool {
    match event {
        Event::DefaultChanged(flow, role) => follow.device.is_empty() && flow == follow.flow && role == eConsole,
        Event::Activated(id) => !follow.device.is_empty() && id == &*follow.device,
    }
}

/// Sets `reopen` when [`wants_reopen`] says so. Windows calls it on its own thread; it only
/// stores a flag, since callbacks must not block or call back into the enumerator.
#[implement(IMMNotificationClient)]
struct Watcher {
    follow: Follow,
    reopen: Arc<AtomicBool>,
}

impl Watcher {
    fn on(&self, event: Event) {
        if wants_reopen(&self.follow, event) {
            self.reopen.store(true, Ordering::Relaxed);
        }
    }
}

impl IMMNotificationClient_Impl for Watcher_Impl {
    fn OnDeviceStateChanged(&self, id: &PCWSTR, state: DEVICE_STATE) -> Result<()> {
        if state == DEVICE_STATE_ACTIVE && !id.is_null() {
            // SAFETY: Windows passes a NUL-terminated id that stays valid during the call.
            self.on(Event::Activated(unsafe { id.as_wide() }));
        }
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        self.on(Event::DefaultChanged(flow, role));
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// A registered [`Watcher`]. Dropping it unregisters before the client is released (the
/// enumerator holds no reference of its own), so keep it on the thread that registered it and
/// drop it before `CoUninitialize`.
pub(super) struct Watch {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl Watch {
    pub(super) fn register(follow: Follow, reopen: Arc<AtomicBool>) -> Result<Self> {
        let client: IMMNotificationClient = Watcher { follow, reopen }.into();
        // SAFETY: COM calls from an MTA thread; `client` lives in `Watch` until unregistered.
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            enumerator.RegisterEndpointNotificationCallback(&client)?;
            // Microsoft doesn't say Unregister waits for a callback already running, so one
            // reference outlives it and a late callback never touches freed memory.
            // shortcut: leaks a few bytes per source start; drop it if Microsoft documents the wait.
            std::mem::forget(client.clone());
            Ok(Watch { enumerator, client })
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: unregisters the client registered in `register`, which is still alive.
        let _ = unsafe { self.enumerator.UnregisterEndpointNotificationCallback(&self.client) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn follow(flow: EDataFlow, device: &str) -> Follow {
        Follow { flow, device: device.into() }
    }

    #[test]
    fn the_default_stream_follows_only_its_own_console_default() {
        let desktop = follow(eRender, "");
        assert!(wants_reopen(&desktop, Event::DefaultChanged(eRender, eConsole)));
        // Windows sends one call per role; only the role we opened counts, once.
        assert!(!wants_reopen(&desktop, Event::DefaultChanged(eRender, eMultimedia)));
        assert!(!wants_reopen(&desktop, Event::DefaultChanged(eRender, eCommunications)));
        assert!(!wants_reopen(&desktop, Event::DefaultChanged(eCapture, eConsole)), "a new default mic");
        let id: Vec<u16> = "{0.0.0.00000000}.{abc}".encode_utf16().collect();
        assert!(!wants_reopen(&desktop, Event::Activated(&id)), "the default moves by its own event");
        let mic = follow(eCapture, "");
        assert!(wants_reopen(&mic, Event::DefaultChanged(eCapture, eConsole)));
        assert!(!wants_reopen(&mic, Event::DefaultChanged(eRender, eConsole)));
    }

    #[test]
    fn a_chosen_device_reopens_when_it_comes_back() {
        let mic = follow(eCapture, "{0.0.1.00000000}.{abc}");
        let wide = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert!(wants_reopen(&mic, Event::Activated(&wide("{0.0.1.00000000}.{abc}"))));
        assert!(!wants_reopen(&mic, Event::Activated(&wide("{0.0.1.00000000}.{def}"))));
        assert!(!wants_reopen(&mic, Event::DefaultChanged(eCapture, eConsole)), "the user picked this one");
    }

    /// Hardware tier: lists the microphones and records half a second from each.
    #[test]
    #[ignore = "needs audio devices"]
    fn lists_and_opens_each_microphone() {
        use super::super::microphone::Microphone;
        let mics = microphones().unwrap();
        assert!(!mics.is_empty(), "no active microphone");
        for mic in mics {
            let frames = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let count = frames.clone();
            let mut source = Microphone::start(&mic.id, move |s, _| {
                count.fetch_add(s.len() as u64 / 2, Ordering::Relaxed);
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
            source.stop();
            let stats = source.stats();
            println!("{} [{}]: {} frames, {stats:?}", mic.name, mic.id, frames.load(Ordering::Relaxed));
            assert_eq!(stats.device_errors, 0, "{}: {:?}", mic.name, stats.last_error);
            assert!(frames.load(Ordering::Relaxed) > 0, "{}", mic.name);
        }
    }

    /// Hardware tier: every active endpoint accepts Backsight's 48 kHz float stereo request
    /// (`AUTOCONVERTPCM`) whatever its own mix format, which this prints.
    #[test]
    #[ignore = "needs audio devices"]
    fn every_endpoint_converts_to_48k_stereo() {
        use super::super::wasapi::Device;
        // SAFETY: test-thread COM setup and calls on live objects; the mix format is read before
        // CoTaskMemFree releases it.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            for (flow, flags) in [(eRender, AUDCLNT_STREAMFLAGS_LOOPBACK), (eCapture, 0)] {
                let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE).unwrap();
                for i in 0..devices.GetCount().unwrap() {
                    let device = devices.Item(i).unwrap();
                    let name = device.OpenPropertyStore(STGM_READ).unwrap().GetValue(&PKEY_Device_FriendlyName);
                    let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
                    let mix = client.GetMixFormat().unwrap();
                    let (rate, channels, bits) = ((*mix).nSamplesPerSec, (*mix).nChannels, (*mix).wBitsPerSample);
                    CoTaskMemFree(Some(mix as _));
                    let opened = Device::start(client, flags).map(drop);
                    println!("{flow:?} {}: mix {rate} Hz {channels} ch {bits} bit -> {opened:?}", name.unwrap());
                    assert!(opened.is_ok());
                }
            }
        }
    }
}
