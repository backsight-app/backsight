//! Windows Imaging Component JPEG XR encoding for linear scRGB RGBA half-floats.
use std::ptr::null;
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, HGLOBAL, RPC_E_CHANGED_MODE, VARIANT_BOOL},
        Graphics::Imaging::*,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
                STREAM_SEEK_END, STREAM_SEEK_SET,
                StructuredStorage::{CreateStreamOnHGlobal, PROPBAG2},
            },
            Variant::{VARIANT, VT_BOOL},
        },
    },
    core::{Error, PWSTR, Result},
};

struct Apartment(bool);

impl Apartment {
    fn enter() -> Result<Self> {
        // SAFETY: A successful call is balanced on this thread after all WIC objects drop.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self(false)); // COM is already initialized in the caller's apartment.
        }
        result.ok()?;
        Ok(Self(true))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: Balances this thread's successful CoInitializeEx, including S_FALSE.
            unsafe { CoUninitialize() };
        }
    }
}

/// Encodes tightly packed, top-down, little-endian scRGB RGBA half-float pixels losslessly.
pub fn encode(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let Some(stride) = width.checked_mul(8) else {
        return Err(Error::new(E_INVALIDARG, "JPEG XR row is too wide."));
    };
    if width == 0 || height == 0 || stride.checked_mul(height).map(|n| n as usize) != Some(rgba.len()) {
        return Err(Error::new(E_INVALIDARG, "Invalid JPEG XR frame dimensions."));
    }
    let _apartment = Apartment::enter()?;
    // SAFETY: WIC interfaces and stream are local to this COM apartment. Out-pointers refer to
    // writable locals, the property name and variant live through Write, and rgba has height rows.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatWmp, null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let (mut frame, mut options) = (None, None);
        encoder.CreateNewFrame(&mut frame, &mut options)?;
        let frame = frame.ok_or_else(|| Error::new(E_FAIL, "JPEG XR encoder returned no frame."))?;
        let options = options.ok_or_else(|| Error::new(E_FAIL, "JPEG XR encoder returned no options."))?;
        let mut name: Vec<u16> = "Lossless\0".encode_utf16().collect();
        let property = PROPBAG2 { pstrName: PWSTR(name.as_mut_ptr()), ..Default::default() };
        let mut lossless = VARIANT::default();
        let value = &mut *lossless.Anonymous.Anonymous;
        value.vt = VT_BOOL;
        value.Anonymous.boolVal = VARIANT_BOOL(-1);
        options.Write(1, &property, &lossless)?;
        frame.Initialize(&options)?;
        frame.SetSize(width, height)?;
        let mut format = GUID_WICPixelFormat64bppRGBAHalf;
        frame.SetPixelFormat(&mut format)?;
        if format != GUID_WICPixelFormat64bppRGBAHalf {
            return Err(Error::new(E_FAIL, "JPEG XR encoder did not accept half-float pixels."));
        }
        frame.WritePixels(height, stride, rgba)?;
        frame.Commit()?;
        encoder.Commit()?;

        let mut len = 0;
        stream.Seek(0, STREAM_SEEK_END, Some(&mut len))?;
        let len = u32::try_from(len).map_err(|_| Error::new(E_FAIL, "JPEG XR file is too large."))?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0; len as usize];
        let mut read = 0;
        stream.Read(bytes.as_mut_ptr().cast(), len, Some(&mut read)).ok()?;
        if read != len || len == 0 {
            return Err(Error::new(E_FAIL, "JPEG XR stream is incomplete."));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_round_trip_preserves_extended_range_and_channel_order() {
        // Half-float pixels with values above 1 and below 0, as produced by scRGB.
        let pixels: Vec<u8> = [0x4000u16, 0xb400, 0x3800, 0x3c00, 0x3000, 0x4400, 0x0000, 0x3c00]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(encode(&pixels, 1, 1).is_err());
        let bytes = encode(&pixels, 2, 1).unwrap();
        let _apartment = Apartment::enter().unwrap();
        // SAFETY: COM interfaces stay in this apartment and the input buffer outlives the decoder.
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let stream = factory.CreateStream().unwrap();
            stream.InitializeFromMemory(&bytes).unwrap();
            let decoder = factory.CreateDecoderFromStream(&stream, null(), WICDecodeMetadataCacheOnLoad).unwrap();
            assert_eq!(decoder.GetContainerFormat().unwrap(), GUID_ContainerFormatWmp);
            let frame = decoder.GetFrame(0).unwrap();
            assert_eq!(frame.GetPixelFormat().unwrap(), GUID_WICPixelFormat64bppRGBAHalf);
            let mut decoded = [0; 16];
            frame.CopyPixels(null(), 16, &mut decoded).unwrap();
            assert_eq!(decoded.as_slice(), pixels);
        }
    }
}
