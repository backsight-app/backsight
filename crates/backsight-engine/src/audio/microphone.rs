//! Microphone: WASAPI shared-mode capture of one capture endpoint (or the Windows default),
//! delivered in the same format and on the same clock as desktop loopback. Gotchas are in
//! `docs/reference/platform-notes.md`.

use super::{
    devices::Follow,
    wasapi::{Device, Loopback, LoopbackStats},
};
use windows::{
    Win32::{
        Foundation::E_ACCESSDENIED,
        Media::Audio::*,
        System::Com::{CLSCTX_ALL, CoCreateInstance},
    },
    core::{Error, HRESULT, HSTRING, Result},
};

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: no capture device, or an unknown endpoint id.
const E_NOTFOUND: HRESULT = HRESULT(0x80070490_u32 as i32);

/// Microphone capture on its own thread; dropping it stops the thread.
pub struct Microphone {
    stream: Loopback,
}

impl Microphone {
    /// Captures `device` (an endpoint id from settings' `audio.microphone_device`; `""` or
    /// `"default"` follows the Windows default microphone) and calls `on_audio` exactly as
    /// [`Loopback::start`] does: [`RATE`](super::wasapi::RATE) Hz [`CHANNELS`](super::wasapi::CHANNELS)
    /// float, continuous on the QPC clock. A mono mic comes out on both channels.
    ///
    /// Never fails: while the mic can't be opened (blocked, unplugged) the stream is silence,
    /// it retries every 500 ms and [`LoopbackStats::problem`] says what to do. On the Windows
    /// default it moves to a new default at once; a chosen mic reopens as soon as it is plugged
    /// back in. A missing chosen mic is never swapped for another one: that would record the
    /// wrong person's voice without saying so.
    pub fn start(device: &str, on_audio: impl FnMut(&[f32], u64) + Send + 'static) -> Self {
        let device = HSTRING::from(if device == "default" { "" } else { device });
        let follow = Follow { flow: eCapture, device: device.clone() };
        let open = move || open(&device);
        Microphone { stream: Loopback::spawn("backsight-microphone", Some(follow), open, explain, on_audio) }
    }

    pub fn stats(&self) -> LoopbackStats {
        self.stream.stats()
    }

    /// Stops and joins the thread; [`Microphone::stats`] keeps the final counts afterwards.
    pub fn stop(&mut self) {
        self.stream.stop();
    }
}

fn open(device: &HSTRING) -> Result<Device> {
    // SAFETY: COM calls on live objects from the MTA audio thread; `device` outlives GetDevice.
    let client = unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoint = if device.is_empty() {
            enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?
        } else {
            enumerator.GetDevice(device)?
        };
        endpoint.Activate(CLSCTX_ALL, None)?
    };
    Device::start(client, 0)
}

/// What to tell the user when the microphone can't be opened. The raw error goes to the log.
fn explain(e: &Error) -> &'static str {
    match e.code() {
        // Initialize fails this way when Settings > Privacy & security > Microphone blocks us.
        E_ACCESSDENIED => {
            "Windows is blocking the microphone. In Windows Settings > Privacy & security > Microphone, turn on \
             Microphone access and Let desktop apps access your microphone."
        }
        E_NOTFOUND | AUDCLNT_E_DEVICE_INVALIDATED => {
            "No microphone found. Plug it in, or pick another on Backsight's Audio page; Backsight keeps trying."
        }
        AUDCLNT_E_DEVICE_IN_USE => {
            "Another app has the microphone to itself. Close it, or in Windows Sound settings turn off \
             exclusive control for this microphone."
        }
        _ => "The microphone stopped working. Check it is plugged in; Backsight keeps trying.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_FAIL;

    #[test]
    fn errors_are_explained_in_plain_words() {
        let say = |code| explain(&Error::from(code));
        assert!(say(E_ACCESSDENIED).contains("Privacy & security > Microphone"));
        assert!(say(E_NOTFOUND).starts_with("No microphone found"));
        assert_eq!(say(AUDCLNT_E_DEVICE_INVALIDATED), say(E_NOTFOUND), "unplugged while recording");
        assert!(say(AUDCLNT_E_DEVICE_IN_USE).contains("exclusive"));
        assert!(say(E_FAIL).starts_with("The microphone stopped working"));
    }
}
