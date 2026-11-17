//! The one D3D11 device that capture, colour conversion and the encoder share.

use backsight_encode::session::D3d11Device;
use std::ffi::c_void;
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{Direct3D::*, Direct3D11::*, Dxgi::*},
    },
    core::{Interface, Result},
};

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub vendor_id: u32,
    pub name: String,
    multithread: ID3D11Multithread,
}

/// Every hardware adapter's vendor and user-mode driver version, e.g. `10de:32.0.15.6094 8086:…`.
/// The encoder probe is cached under this key, so updating any GPU driver re-probes (Quick Sync
/// runs on the Intel iGPU even when capture uses the NVIDIA card).
pub fn drivers_key() -> Result<String> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut parts = Vec::new();
    for i in 0.. {
        // SAFETY: COM call on a live factory; fails once `i` passes the last adapter.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        // SAFETY: COM calls on a live adapter. CheckInterfaceSupport with IDXGIDevice's IID
        // returns the UMD version (WARP reports one too; a failure reads as 0.0.0.0).
        let (desc, version) = unsafe { (adapter.GetDesc1()?, adapter.CheckInterfaceSupport(&IDXGIDevice::IID)) };
        let v = version.unwrap_or_default() as u64;
        parts.push(format!(
            "{:x}:{}.{}.{}.{}",
            desc.VendorId,
            v >> 48,
            (v >> 32) & 0xFFFF,
            (v >> 16) & 0xFFFF,
            v & 0xFFFF
        ));
    }
    parts.sort(); // enumeration order changes with docking and which GPU drives the display
    Ok(parts.join(" "))
}

/// Every hardware adapter as (PCI vendor id, name), e.g. `(0x10DE, "NVIDIA GeForce RTX ...")`.
/// Software adapters (WARP) are left out.
pub fn graphics_cards() -> Result<Vec<(u32, String)>> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut cards = Vec::new();
    for i in 0.. {
        // SAFETY: COM calls on a live factory and adapter; EnumAdapters1 fails past the last one.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        // SAFETY: as above.
        let desc = unsafe { adapter.GetDesc1() }?;
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len]);
        cards.push((desc.VendorId, name));
    }
    Ok(cards)
}

/// PCI vendor id for the catalog's `vendor` field (`cpu` has none).
pub fn pci_vendor(vendor: &str) -> Option<u32> {
    match vendor {
        "nvidia" => Some(0x10DE),
        "amd" => Some(0x1002),
        "intel" => Some(0x8086),
        _ => None,
    }
}

impl Gpu {
    /// A device on the adapter made by `vendor`, else on the next adapter in high-performance
    /// order (the dGPU first on a hybrid laptop) that opens, else WARP. Frames then reach the
    /// encoder without leaving that GPU.
    pub fn open(vendor: &str) -> Result<Gpu> {
        // SAFETY: plain factory creation.
        let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory1() }?;
        let want = pci_vendor(vendor);
        let mut adapters = Vec::new();
        for i in 0.. {
            // SAFETY: COM call on a live factory; fails once `i` passes the last adapter.
            let Ok(adapter) = (unsafe {
                factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
            }) else {
                break;
            };
            // SAFETY: COM call on a live adapter.
            let desc = unsafe { adapter.GetDesc1() }?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            adapters.push((desc.VendorId, adapter));
        }
        adapters.sort_by_key(|(id, _)| want != Some(*id)); // stable: keeps the preference order
        for (id, adapter) in adapters {
            match Self::create(Some(&adapter.cast()?), D3D_DRIVER_TYPE_UNKNOWN) {
                Ok(gpu) => return Ok(gpu),
                Err(e) => tracing::warn!("no D3D11 device on adapter {id:#06x}, trying the next: {e}"),
            }
        }
        Self::warp()
    }

    /// Microsoft's software rasterizer: works without a GPU (CI, remote desktops), but has no
    /// video processor, so [`super::convert::Converter`] can't run on it.
    pub fn warp() -> Result<Gpu> {
        Self::create(None, D3D_DRIVER_TYPE_WARP)
    }

    fn create(adapter: Option<&IDXGIAdapter>, driver_type: D3D_DRIVER_TYPE) -> Result<Gpu> {
        let (mut device, mut context) = (None, None);
        // BGRA support is required by WGC and video support by ID3D11VideoProcessor
        // (platform-notes.md). WARP has no video support and fails with DXGI_ERROR_UNSUPPORTED.
        let flags = match driver_type {
            D3D_DRIVER_TYPE_WARP => D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            _ => D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        };
        // SAFETY: out-pointers are valid locals.
        unsafe {
            D3D11CreateDevice(
                adapter,
                driver_type,
                HMODULE::default(),
                flags,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }?;
        let device: ID3D11Device = device.expect("D3D11CreateDevice succeeded");
        let multithread: ID3D11Multithread = device.cast()?;
        // SAFETY: COM calls on the live device; capture, conversion and FFmpeg use it from
        // different threads, so its immediate context must take a lock.
        let adapter = unsafe {
            let _ = multithread.SetMultithreadProtected(true); // returns the previous setting
            device.cast::<IDXGIDevice>()?.GetAdapter()?
        };
        // SAFETY: COM call on a live adapter.
        let desc = unsafe { adapter.GetDesc() }?;
        let len = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len]);
        crate::crash::note_gpu(&name);
        Ok(Gpu {
            device,
            context: context.expect("D3D11CreateDevice succeeded"),
            vendor_id: desc.VendorId,
            name,
            multithread,
        })
    }

    /// Holds the device lock until the guard drops. FFmpeg's NVENC encoder uses our device
    /// inside `send` and `receive` without taking the lock (platform-notes.md), so wrap those.
    pub fn lock(&self) -> DeviceLock<'_> {
        // SAFETY: COM call on the live lock object; DeviceLock's drop balances it.
        unsafe { self.multithread.Enter() };
        DeviceLock(&self.multithread)
    }

    /// The device for FFmpeg, sharing this device's lock.
    /// The session keeps the lock object alive through `keepalive`.
    pub fn for_ffmpeg(&self) -> D3d11Device {
        D3d11Device {
            // FFmpeg releases this reference when its device context is freed.
            device: self.device.clone().into_raw(),
            lock: lock_cb,
            unlock: unlock_cb,
            lock_ctx: self.multithread.as_raw(),
            keepalive: Box::new(self.multithread.clone()),
        }
    }
}

pub struct DeviceLock<'a>(&'a ID3D11Multithread);

impl Drop for DeviceLock<'_> {
    fn drop(&mut self) {
        // SAFETY: balances the Enter in Gpu::lock on the same thread.
        unsafe { self.0.Leave() }
    }
}

unsafe extern "C" fn lock_cb(ctx: *mut c_void) {
    // SAFETY: FFmpeg passes back `lock_ctx`, a live ID3D11Multithread (see Gpu::for_ffmpeg).
    unsafe { ID3D11Multithread::from_raw_borrowed(&ctx).expect("lock_ctx is set").Enter() }
}

unsafe extern "C" fn unlock_cb(ctx: *mut c_void) {
    // SAFETY: as in lock_cb.
    unsafe { ID3D11Multithread::from_raw_borrowed(&ctx).expect("lock_ctx is set").Leave() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_device_even_without_a_gpu() {
        let gpu = Gpu::open("cpu").expect("a hardware adapter or WARP");
        assert!(!gpu.name.is_empty());
        let warp = Gpu::warp().unwrap();
        assert!(warp.name.contains("Microsoft"), "{}", warp.name);
    }

    #[test]
    fn drivers_key_lists_every_adapter() {
        let key = drivers_key().unwrap();
        println!("drivers: {key}");
        // At least the Microsoft Basic Render Driver (vendor 1414), present on every Windows PC.
        let parts: Vec<_> = key.split(' ').collect();
        assert!(parts.iter().all(|p| p.contains(':')), "{key}");
        assert!(key.contains("1414:"), "{key}");
        // Every adapter, WARP included, reports a driver version; 0.0.0.0 means the query broke.
        assert!(parts.iter().all(|p| !p.ends_with(":0.0.0.0")), "{key}");
        assert!(parts.is_sorted(), "{key}");
    }
}
