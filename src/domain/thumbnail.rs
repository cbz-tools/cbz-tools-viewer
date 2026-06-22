use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub width: u16,
    pub height: u16,
    pub pixels: Arc<[u8]>, // RGBA8、zero-copy 共有
}
