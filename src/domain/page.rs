#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageFormatHint {
    Jpeg,
    Png,
    WebP,
    Gif,
    Avif,
    Unknown,
}

impl ImageFormatHint {
    /// magic number から形式判定（先頭 12 バイト）
    pub fn from_magic(buf: &[u8]) -> Self {
        match buf {
            [0xFF, 0xD8, 0xFF, ..] => Self::Jpeg,
            [0x89, b'P', b'N', b'G', ..] => Self::Png,
            // RIFF????WEBP — `..` で 12 バイト超のファイルにも対応
            [
                b'R',
                b'I',
                b'F',
                b'F',
                _,
                _,
                _,
                _,
                b'W',
                b'E',
                b'B',
                b'P',
                ..,
            ] => Self::WebP,
            [b'G', b'I', b'F', b'8', ..] => Self::Gif,
            _ => {
                // AVIF: ISOBMFF コンテナ（bytes[4..8] == "ftyp"、brand == "avif"/"avis" 等）
                if buf.len() >= 12 && &buf[4..8] == b"ftyp" {
                    let brand = &buf[8..12];
                    if brand == b"avif" || brand == b"avis" || brand == b"MA1A" || brand == b"MA1B"
                    {
                        return Self::Avif;
                    }
                }
                Self::Unknown
            }
        }
    }
}
