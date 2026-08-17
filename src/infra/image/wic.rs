use anyhow::{Result, bail};

use super::decode::DecodedImage;

#[cfg(windows)]
fn decode_cmyk_ycck_jpeg_impl(data: &[u8]) -> Result<DecodedImage> {
    use std::ptr;

    use anyhow::Context;
    use windows::Win32::{
        Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
            WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnLoad,
        },
        System::{Com::CLSCTX_INPROC_SERVER, Com::CoCreateInstance},
        UI::Shell::SHCreateMemStream,
    };

    let _com =
        crate::platform::com::ComApartment::new().context("COM apartment initialization failed")?;
    let stream = unsafe { SHCreateMemStream(Some(data)) }
        .ok_or_else(|| anyhow::anyhow!("SHCreateMemStream returned null"))?;
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .context("CoCreateInstance(WIC imaging factory)")?;
    let decoder = unsafe {
        factory.CreateDecoderFromStream(&stream, ptr::null(), WICDecodeMetadataCacheOnLoad)
    }
    .context("CreateDecoderFromStream")?;
    let frame = unsafe { decoder.GetFrame(0) }.context("GetFrame(0)")?;
    let converter = unsafe { factory.CreateFormatConverter() }.context("CreateFormatConverter")?;
    unsafe {
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .context("IWICFormatConverter::Initialize")?;
    }

    let mut width = 0u32;
    let mut height = 0u32;
    unsafe { converter.GetSize(&mut width, &mut height) }.context("WIC GetSize")?;
    let stride = width
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("WIC stride overflow"))?;
    let buffer_len = usize::try_from(stride)
        .ok()
        .and_then(|stride| usize::try_from(height).ok()?.checked_mul(stride))
        .ok_or_else(|| anyhow::anyhow!("WIC pixel buffer size overflow"))?;
    let mut bgra = vec![0u8; buffer_len];
    unsafe { converter.CopyPixels(ptr::null(), stride, &mut bgra) }.context("WIC CopyPixels")?;

    Ok(DecodedImage {
        width,
        height,
        pixels: bgra_to_rgba(bgra)?,
    })
}

pub(super) fn decode_cmyk_ycck_jpeg(data: &[u8]) -> Result<DecodedImage> {
    #[cfg(windows)]
    {
        decode_cmyk_ycck_jpeg_impl(data)
    }

    #[cfg(not(windows))]
    {
        let _ = data;
        bail!("WIC CMYK/YCCK JPEG fallback is only available on Windows");
    }
}

fn bgra_to_rgba(mut pixels: Vec<u8>) -> Result<Vec<u8>> {
    if pixels.len() % 4 != 0 {
        bail!("WIC BGRA buffer length is not a multiple of four");
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(pixels)
}
