/// 見開き時は1ページあたり半幅、単ページ時は全幅を使う。
pub(super) fn request_display_width_for_pair(display_w: u32, has_right_page: bool) -> u32 {
    if has_right_page {
        display_w.div_ceil(2).max(1)
    } else {
        display_w
    }
}

/// 指定decode寸法のstatic RGBAページ群に必要な予測byte数。
pub(super) fn static_rgba_bytes_for_decode(
    decode_w: u32,
    decode_h: u32,
    page_count: usize,
) -> usize {
    (decode_w as usize)
        .saturating_mul(decode_h as usize)
        .saturating_mul(4)
        .saturating_mul(page_count)
}
