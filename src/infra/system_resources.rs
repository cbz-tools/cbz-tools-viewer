use crate::domain::performance::PerformanceResources;

pub fn detect_pc_resources() -> PerformanceResources {
    let logical_cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .max(1);

    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        use windows::Win32::Graphics::Dxgi::{
            CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory6,
            DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
        };
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

        let physical_ram_bytes = {
            let mut memory = MEMORYSTATUSEX {
                dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
                ..Default::default()
            };
            // SAFETY: `MEMORYSTATUSEX` は `dwLength` を設定済みで、Win32 は失敗時に 0 を返すだけ。
            unsafe {
                if GlobalMemoryStatusEx(&mut memory).is_ok() {
                    memory.ullTotalPhys
                } else {
                    0
                }
            }
        };

        let mut best_adapter_name: Option<String> = None;
        let mut best_dedicated_vram_bytes: Option<u64> = None;

        // SAFETY: DXGI factory 生成は所有権付き COM wrapper を返し、失敗は `Err` で扱う。
        if let Ok(factory) = unsafe { CreateDXGIFactory1::<IDXGIFactory6>() } {
            let mut index = 0u32;
            loop {
                // SAFETY: `index` はこのループで単調増加し、列挙失敗で終了する。
                let Ok(adapter) = (unsafe {
                    factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                        index,
                        DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                    )
                }) else {
                    break;
                };
                // SAFETY: `adapter` は直前の DXGI 列挙成功値。
                let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                    index = index.saturating_add(1);
                    continue;
                };
                if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                    index = index.saturating_add(1);
                    continue;
                }
                let adapter_name = {
                    let len = desc
                        .Description
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.Description.len());
                    OsString::from_wide(&desc.Description[..len])
                        .to_string_lossy()
                        .into_owned()
                };
                best_adapter_name = Some(adapter_name);
                best_dedicated_vram_bytes = Some(desc.DedicatedVideoMemory as u64);
                break;
            }
        } else if let Ok(factory) = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
            let mut index = 0u32;
            loop {
                // SAFETY: `index` はこのループで単調増加し、列挙失敗で終了する。
                let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                    break;
                };
                // SAFETY: `adapter` は直前の DXGI 列挙成功値。
                let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                    index = index.saturating_add(1);
                    continue;
                };
                if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                    index = index.saturating_add(1);
                    continue;
                }
                let adapter_name = {
                    let len = desc
                        .Description
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.Description.len());
                    OsString::from_wide(&desc.Description[..len])
                        .to_string_lossy()
                        .into_owned()
                };
                best_adapter_name = Some(adapter_name);
                best_dedicated_vram_bytes = Some(desc.DedicatedVideoMemory as u64);
                break;
            }
        }

        PerformanceResources {
            physical_ram_bytes,
            dedicated_vram_bytes: best_dedicated_vram_bytes,
            logical_cpu_count,
            gpu_adapter_name: best_adapter_name,
        }
    }

    #[cfg(not(windows))]
    {
        PerformanceResources {
            physical_ram_bytes: 0,
            dedicated_vram_bytes: None,
            logical_cpu_count,
            gpu_adapter_name: None,
        }
    }
}
