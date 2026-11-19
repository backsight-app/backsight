//! Windows Imaging Component JPEG XR encoding and decoding of linear scRGB RGBA half-floats.
use std::{path::Path, ptr::null};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, GENERIC_READ, HGLOBAL, RPC_E_CHANGED_MODE, VARIANT_BOOL},
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
    core::{Error, HSTRING, Interface, PWSTR, Result},
};

pub(crate) struct Apartment(bool);

impl Apartment {
    pub(crate) fn enter() -> Result<Self> {
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

/// Reads a JPEG XR file as top-down scRGB RGBA half-floats, scaled down to fit `max` (aspect
/// kept, never enlarged). Returns the pixels, width and height.
pub fn decode(path: &Path, max: (u32, u32)) -> Result<(Vec<u8>, u32, u32)> {
    let _apartment = Apartment::enter()?;
    // SAFETY: WIC interfaces are local to this COM apartment; the path string lives through the
    // call and `pixels` holds exactly `stride * h` bytes for CopyPixels.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let name = HSTRING::from(path.as_os_str());
        let decoder = factory.CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let (mut fw, mut fh) = (0, 0);
        frame.GetSize(&mut fw, &mut fh)?;
        // The float conversion and scaler may hold the whole image: 16 bytes a pixel.
        if u64::from(fw) * u64::from(fh) > 64_000_000 {
            return Err(Error::new(E_FAIL, "JPEG XR image is too large for a thumbnail."));
        }
        // WIC's scaler takes full floats but not half-floats: float, scaled, then half again.
        let convert = |source: &IWICBitmapSource, to: &windows::core::GUID| -> Result<IWICBitmapSource> {
            let c = factory.CreateFormatConverter()?;
            c.Initialize(source, to, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            c.cast()
        };
        let float = convert(&frame.cast()?, &GUID_WICPixelFormat128bppRGBAFloat)?;
        let (mut w, mut h) = (0, 0);
        float.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 {
            return Err(Error::new(E_FAIL, "JPEG XR image is empty."));
        }
        let scale = (f64::from(max.0) / f64::from(w)).min(f64::from(max.1) / f64::from(h)).min(1.0);
        let (sw, sh) = (((f64::from(w) * scale) as u32).max(1), ((f64::from(h) * scale) as u32).max(1));
        let scaler = factory.CreateBitmapScaler()?;
        scaler.Initialize(&float, sw, sh, WICBitmapInterpolationModeFant)?;
        let half = convert(&scaler.cast()?, &GUID_WICPixelFormat64bppRGBAHalf)?;
        let stride = sw * 8;
        let mut pixels = vec![0; stride as usize * sh as usize];
        half.CopyPixels(null(), stride, &mut pixels)?;
        Ok((pixels, sw, sh))
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

    #[test]
    fn decodes_a_file_scaled_to_fit() {
        let red: Vec<u8> = [0x4000u16, 0, 0, 0x3c00].into_iter().flat_map(u16::to_le_bytes).collect();
        let bytes = encode(&red.repeat(40 * 20), 40, 20).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.jxr");
        std::fs::write(&path, bytes).unwrap();
        let (pixels, w, h) = decode(&path, (10, 10)).unwrap();
        assert_eq!((w, h, pixels.len()), (10, 5, 10 * 5 * 8));
        // 2.0 as a half-float: brighter than SDR white survives.
        let r = u16::from_le_bytes([pixels[0], pixels[1]]);
        assert!(r.abs_diff(0x4000) < 0x10, "extended range kept: {r:#x}");
        assert!(decode(&dir.path().join("gone.jxr"), (10, 10)).is_err());
    }
}
