use anyhow::Result;

use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
    },
    core::Error as WinError,
};

pub(crate) struct ComApartment {
    should_uninit: bool,
}

impl ComApartment {
    pub(crate) fn new() -> Result<Self> {
        // SAFETY:
        // COM apartment はこのスレッド内だけで初期化し、成功時だけ Drop で対応する。
        // `RPC_E_CHANGED_MODE` は既存 apartment を流用できるので uninit しない。
        unsafe {
            match CoInitializeEx(None, COINIT_APARTMENTTHREADED) {
                hr if hr.is_ok() => Ok(Self {
                    should_uninit: true,
                }),
                hr if hr == RPC_E_CHANGED_MODE => Ok(Self {
                    should_uninit: false,
                }),
                hr => Err(WinError::from(hr).into()),
            }
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.should_uninit {
            // SAFETY: `should_uninit=true` はこの型が `CoInitializeEx` 成功を記録した場合だけ。
            unsafe { CoUninitialize() };
        }
    }
}
