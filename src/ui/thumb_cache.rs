use std::{fmt, path::Path, time::SystemTime};

use eframe::egui;

use crate::{
    domain::archive::BookId,
    infra::{cache::disk::DiskCache, image::decode::decode_webp},
};

#[derive(Clone)]
pub struct LoadedDiskThumb {
    pub texture: egui::TextureHandle,
    pub image_size: [usize; 2],
}

impl fmt::Debug for LoadedDiskThumb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedDiskThumb")
            .field("texture_id", &self.texture.id())
            .field("image_size", &self.image_size)
            .finish()
    }
}

pub fn load_disk_thumb_texture(
    ctx: &egui::Context,
    cache: &DiskCache,
    path: &Path,
    file_size: u64,
    modified: Option<SystemTime>,
    texture_name: impl Into<String>,
) -> Option<LoadedDiskThumb> {
    let id = BookId::from_path(path);
    let bytes = cache.get_thumb(&id, file_size, modified)?;
    let img = decode_webp(&bytes).ok()?;
    let image_size = [img.width as usize, img.height as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(image_size, &img.pixels);
    let texture = ctx.load_texture(texture_name.into(), color, egui::TextureOptions::LINEAR);
    Some(LoadedDiskThumb {
        texture,
        image_size,
    })
}
