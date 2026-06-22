use std::path::PathBuf;

use anyhow::Result;

#[cfg(windows)]
mod imp {
    use std::{
        mem::{size_of, ManuallyDrop},
        os::windows::ffi::OsStrExt,
        path::PathBuf,
        ptr,
    };

    use anyhow::{bail, Context, Result};
    use windows::{
        core::{implement, Error as WinError, BOOL, HRESULT},
        Win32::{
            Foundation::{
                DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP,
                DRAGDROP_S_USEDEFAULTCURSORS, DV_E_DVASPECT, DV_E_FORMATETC, DV_E_TYMED, E_NOTIMPL,
                HWND, OLE_E_ADVISENOTSUPPORTED, RPC_E_CHANGED_MODE, S_OK,
            },
            System::{
                Com::{
                    CoInitializeEx, CoUninitialize, IAdviseSink, IDataObject, IDataObject_Impl,
                    IEnumFORMATETC, IEnumSTATDATA, COINIT_APARTMENTTHREADED, DATADIR_GET,
                    DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
                },
                Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT},
                Ole::{IDropSource, IDropSource_Impl, CF_HDROP, DROPEFFECT_COPY},
                SystemServices::MK_LBUTTON,
            },
            UI::Shell::{SHCreateStdEnumFmtEtc, SHDoDragDrop, DROPFILES},
        },
    };
    // windows-core は直接依存として残す。
    // windows の `#[implement(...)]` 展開は COM/OLE IDataObject/IDropSource 実装で
    // `windows_core` という crate 名を要求し、`windows::core` 経由では代替できない。
    use windows_core::IUnknownImpl;

    pub fn start_file_drag(hwnd: isize, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            bail!("drag source is empty");
        }
        for path in paths {
            if !path.is_file() {
                bail!("drag source is not a file: {}", path.display());
            }
        }

        let _com = ComApartment::new().context("COM apartment initialization failed")?;
        let data: IDataObject = FileDataObject::new(paths).into();
        let source: IDropSource = FileDropSource.into();

        // SAFETY:
        // `hwnd` は eframe から取得した現行ウィンドウ handle をそのまま渡す。
        // `data` / `source` はこの呼び出し中生存し、COM 初期化も同スレッドで済ませている。
        let effect =
            unsafe { SHDoDragDrop(Some(HWND(hwnd as *mut _)), &data, &source, DROPEFFECT_COPY) }
                .context("SHDoDragDrop failed")?;

        tracing::info!(
            effect = effect.0,
            count = paths.len(),
            first = %paths[0].display(),
            "external drag finished"
        );
        Ok(())
    }

    struct ComApartment {
        should_uninit: bool,
    }

    impl ComApartment {
        fn new() -> Result<Self> {
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

    #[implement(IDataObject)]
    struct FileDataObject {
        paths: Vec<PathBuf>,
        format: FORMATETC,
    }

    impl FileDataObject {
        fn new(paths: &[PathBuf]) -> Self {
            Self {
                paths: paths.to_vec(),
                format: FORMATETC {
                    cfFormat: CF_HDROP.0,
                    ptd: ptr::null_mut(),
                    dwAspect: DVASPECT_CONTENT.0,
                    lindex: -1,
                    tymed: TYMED_HGLOBAL.0 as u32,
                },
            }
        }

        fn query_format_etc(&self, format: *const FORMATETC) -> HRESULT {
            if format.is_null() {
                return DV_E_FORMATETC;
            }

            // SAFETY: null は先に除外済みで、Win32 から渡される `FORMATETC` を参照するだけ。
            let format = unsafe { &*format };
            if format.cfFormat != self.format.cfFormat || format.lindex != -1 {
                return DV_E_FORMATETC;
            }
            if format.dwAspect != DVASPECT_CONTENT.0 {
                return DV_E_DVASPECT;
            }
            if (format.tymed & TYMED_HGLOBAL.0 as u32) == 0 {
                return DV_E_TYMED;
            }
            S_OK
        }

        fn build_medium(&self) -> windows::core::Result<STGMEDIUM> {
            let hglobal = create_hdrop(&self.paths)?;
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: hglobal },
                pUnkForRelease: ManuallyDrop::new(None),
            })
        }
    }

    impl IDataObject_Impl for FileDataObject_Impl {
        fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            let hr = self.get_impl().query_format_etc(pformatetcin);
            if hr != S_OK {
                return Err(WinError::from(hr));
            }
            self.get_impl().build_medium()
        }

        fn GetDataHere(
            &self,
            _pformatetc: *const FORMATETC,
            _pmedium: *mut STGMEDIUM,
        ) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
            self.get_impl().query_format_etc(pformatetc)
        }

        fn GetCanonicalFormatEtc(
            &self,
            _pformatectin: *const FORMATETC,
            pformatetcout: *mut FORMATETC,
        ) -> HRESULT {
            if !pformatetcout.is_null() {
                // SAFETY: Win32 呼び出し側が渡した出力バッファで、null は先に除外済み。
                unsafe { (*pformatetcout).ptd = ptr::null_mut() };
            }
            DATA_S_SAMEFORMATETC
        }

        fn SetData(
            &self,
            _pformatetc: *const FORMATETC,
            _pmedium: *const STGMEDIUM,
            _frelease: BOOL,
        ) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
            if dwdirection != DATADIR_GET.0 as u32 {
                return Err(E_NOTIMPL.into());
            }

            // SAFETY: 列挙する `FORMATETC` は固定長スライスで、この呼び出し中生存する。
            unsafe { SHCreateStdEnumFmtEtc(&[self.get_impl().format]) }
        }

        fn DAdvise(
            &self,
            _pformatetc: *const FORMATETC,
            _advf: u32,
            _padvsink: windows::core::Ref<IAdviseSink>,
        ) -> windows::core::Result<u32> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }
    }

    #[implement(IDropSource)]
    struct FileDropSource;

    impl IDropSource_Impl for FileDropSource_Impl {
        fn QueryContinueDrag(
            &self,
            fescapepressed: BOOL,
            grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        ) -> HRESULT {
            if fescapepressed.as_bool() {
                DRAGDROP_S_CANCEL
            } else if (grfkeystate.0 & MK_LBUTTON.0) == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _dweffect: windows::Win32::System::Ole::DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    fn create_hdrop(
        paths: &[PathBuf],
    ) -> windows::core::Result<windows::Win32::Foundation::HGLOBAL> {
        let mut wide: Vec<u16> = Vec::new();
        for path in paths {
            wide.extend(path.as_os_str().encode_wide());
            wide.push(0);
        }
        wide.push(0);

        let total_bytes = size_of::<DROPFILES>() + wide.len() * size_of::<u16>();
        // SAFETY: 要求サイズは `DROPFILES` と UTF-16 配列長から算出している。
        let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total_bytes) }?;

        // SAFETY: `hglobal` は直前の `GlobalAlloc` 成功値で、lock 成功時だけ書き込む。
        let ptr = unsafe { GlobalLock(hglobal) };
        if ptr.is_null() {
            return Err(WinError::from_thread());
        }

        let dropfiles = DROPFILES {
            pFiles: size_of::<DROPFILES>() as u32,
            pt: Default::default(),
            fNC: BOOL(0),
            fWide: BOOL(1),
        };

        // SAFETY:
        // 確保済みバッファへ `DROPFILES` と UTF-16 パス列を書き込む。
        // コピー長は `wide.len()` に一致し、最後に同じ handle を unlock する。
        unsafe {
            (ptr as *mut DROPFILES).write(dropfiles);
            let dst = (ptr as *mut u8).add(size_of::<DROPFILES>()) as *mut u16;
            ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
            let _ = GlobalUnlock(hglobal);
        }

        Ok(hglobal)
    }
}

#[cfg(windows)]
pub fn start_file_drag(hwnd: isize, paths: &[PathBuf]) -> Result<()> {
    imp::start_file_drag(hwnd, paths)
}

#[cfg(not(windows))]
pub fn start_file_drag(_hwnd: isize, _paths: &[PathBuf]) -> Result<()> {
    Ok(())
}
