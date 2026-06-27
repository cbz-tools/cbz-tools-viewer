use std::{collections::HashMap, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};

use crate::domain::page_map::{
    BookPageMap, PageDescriptor, PageFormat, PageImageFormat, SourceRevision,
};
use crate::infra::image::page_map::{
    read_image_metadata, read_image_metadata_lightweight_first, LightweightImageMetadataOutcome,
};

use super::{zip::ZipArchiveCore, BookReader};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EpubImageBook {
    pub root_opf_path: String,
    pub resources: HashMap<String, EpubResource>,
    pub spine: Vec<EpubSpineItem>,
    pub pages: Vec<EpubImagePage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EpubResource {
    pub id: String,
    pub href: String,
    pub full_path: String,
    pub media_type: String,
    pub properties: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EpubSpineItem {
    pub idref: String,
    pub linear: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EpubImagePage {
    pub page_index: u32,
    pub xhtml_path: String,
    pub image_path: String,
    pub media_type: Option<String>,
}

pub struct EpubImageReader {
    core: ZipArchiveCore,
    book: EpubImageBook,
}

pub(crate) enum EpubPageMapFastOutcome {
    Ready(BookPageMap),
    RequiresComplete,
}

pub(crate) struct EpubPageMapSlowFailure {
    pub page_index: Option<u32>,
    pub image_path: Option<String>,
}

impl EpubImageReader {
    pub fn open(path: &Path) -> Result<Self> {
        let core = ZipArchiveCore::open(path)?;
        fail_if_epub_encrypted(&core)?;
        let book = EpubImageBook::from_core(&core)?;
        if book.pages.is_empty() {
            bail!("image pages not found");
        }
        Ok(Self { core, book })
    }

    pub fn page_count(&self) -> u32 {
        self.book.pages.len() as u32
    }

    pub fn read_page_n(&self, page_n: u32) -> Result<Bytes> {
        let page = self.book.pages.get(page_n as usize).with_context(|| {
            format!(
                "page {page_n} out of range (total {})",
                self.book.pages.len()
            )
        })?;
        self.core.read_entry_by_name(&page.image_path)
    }

    pub(crate) fn book(&self) -> &EpubImageBook {
        &self.book
    }

    pub(crate) fn page_display_labels(&self) -> Vec<String> {
        self.book
            .pages()
            .iter()
            .map(|page| {
                page.image_path
                    .rsplit(['/', '\\'])
                    .find(|part| !part.is_empty())
                    .unwrap_or(page.image_path.as_str())
                    .to_owned()
            })
            .collect()
    }
}

impl BookReader for EpubImageReader {
    fn read_first_image(&self) -> Result<Bytes> {
        self.read_page_n(0)
    }

    fn page_count(&self) -> u32 {
        self.page_count()
    }

    fn read_page_n(&self, n: u32) -> Result<Bytes> {
        self.read_page_n(n)
    }
}

impl EpubImageBook {
    pub(crate) fn from_core(core: &ZipArchiveCore) -> Result<Self> {
        fail_if_epub_encrypted(core)?;
        let container_xml = core
            .read_entry_by_name("META-INF/container.xml")
            .context("container.xml missing")?;
        let root_opf_path = parse_container_xml(&container_xml)?;
        let opf_xml = core
            .read_entry_by_name(&root_opf_path)
            .with_context(|| format!("opf missing: {}", root_opf_path))?;
        let (resources, spine) = parse_opf(&root_opf_path, &opf_xml)?;
        let pages = build_pages(core, &resources, &spine)?;

        Ok(Self {
            root_opf_path,
            resources,
            spine,
            pages,
        })
    }

    pub(crate) fn pages(&self) -> &[EpubImagePage] {
        &self.pages
    }
}

fn fail_if_epub_encrypted(core: &ZipArchiveCore) -> Result<()> {
    // 暗号化/DRM EPUB は早期に恒久失敗へ寄せる。専用の代替処理は持たない。
    if core
        .find_entry_index_by_name("META-INF/encryption.xml")
        .is_some()
    {
        tracing::warn!(
            "epub encrypted/DRM package is not supported: META-INF/encryption.xml found"
        );
        bail!("epub encrypted/DRM package is not supported: META-INF/encryption.xml found");
    }
    Ok(())
}

pub(crate) fn build_book_page_map_fast_from_epub_reader(
    reader: &EpubImageReader,
    revision: SourceRevision,
) -> EpubPageMapFastOutcome {
    let mut pages = Vec::with_capacity(reader.page_count() as usize);

    for page in reader.book().pages() {
        let Some(format_hint) = page_image_format_hint(page) else {
            return EpubPageMapFastOutcome::RequiresComplete;
        };
        let format_hint = match format_hint {
            PageFormat::Jpeg | PageFormat::Png => format_hint,
            _ => return EpubPageMapFastOutcome::RequiresComplete,
        };
        let raw = match reader.read_page_n(page.page_index) {
            Ok(raw) => raw,
            Err(_) => return EpubPageMapFastOutcome::RequiresComplete,
        };
        let (format, width, height) =
            match read_image_metadata_lightweight_first(&raw, Some(format_hint)) {
                LightweightImageMetadataOutcome::Ready {
                    format,
                    width,
                    height,
                } => (format, width, height),
                LightweightImageMetadataOutcome::FallbackRequired
                | LightweightImageMetadataOutcome::Unsupported => {
                    return EpubPageMapFastOutcome::RequiresComplete;
                }
            };
        pages.push(PageDescriptor {
            format,
            width,
            height,
        });
    }

    EpubPageMapFastOutcome::Ready(BookPageMap::new(revision, pages))
}

pub(crate) fn build_book_page_map_slow_from_epub_path(
    path: &Path,
    revision: SourceRevision,
) -> Result<BookPageMap, EpubPageMapSlowFailure> {
    let reader = EpubImageReader::open(path).map_err(|_| EpubPageMapSlowFailure {
        page_index: None,
        image_path: None,
    })?;
    let mut pages = Vec::with_capacity(reader.page_count() as usize);

    for page in reader.book().pages() {
        let raw = reader
            .read_page_n(page.page_index)
            .map_err(|_| EpubPageMapSlowFailure {
                page_index: Some(page.page_index),
                image_path: Some(page.image_path.clone()),
            })?;
        let (format, width, height) = match read_image_metadata(&raw) {
            Ok(Some(meta)) => meta,
            Ok(None) | Err(_) => {
                return Err(EpubPageMapSlowFailure {
                    page_index: Some(page.page_index),
                    image_path: Some(page.image_path.clone()),
                })
            }
        };
        pages.push(PageDescriptor {
            format,
            width,
            height,
        });
    }

    Ok(BookPageMap::new(revision, pages))
}

fn build_pages(
    core: &ZipArchiveCore,
    resources: &HashMap<String, EpubResource>,
    spine: &[EpubSpineItem],
) -> Result<Vec<EpubImagePage>> {
    let mut pages = Vec::new();

    // EPUB は spine 順をそのまま読書順として使う。linear="no" も今は除外しない。
    // cover / 空ページ / 扉 / 広告が落ちると欠ページに見えるため。
    for spine_item in spine {
        let resource = resources
            .get(&spine_item.idref)
            .with_context(|| format!("spine itemref missing in manifest: {}", spine_item.idref))?;
        let xhtml_bytes = core
            .read_entry_by_name(&resource.full_path)
            .with_context(|| format!("xhtml entry missing: {}", resource.full_path))?;
        let image_paths = extract_xhtml_image_paths(&resource.full_path, &xhtml_bytes)?;

        for image_path in image_paths {
            let image_entry = core
                .find_entry_index_by_name(&image_path)
                .with_context(|| format!("image entry missing: {}", image_path))?;
            let image_resource = find_resource_by_full_path(resources, &image_path);
            if !is_supported_image_path(
                &image_path,
                image_resource.map(|resource| resource.media_type.as_str()),
            ) {
                continue;
            }
            if core.entry(image_entry).is_none() {
                bail!("image entry missing: {image_path}");
            }
            pages.push(EpubImagePage {
                page_index: pages.len() as u32,
                xhtml_path: resource.full_path.clone(),
                image_path,
                media_type: image_resource.map(|resource| resource.media_type.clone()),
            });
        }
    }

    if pages.is_empty() {
        bail!("image pages not found");
    }

    Ok(pages)
}

fn parse_container_xml(xml: &[u8]) -> Result<String> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) | Event::Empty(e) if local_name(e.name().as_ref()) == b"rootfile" => {
                let Some(full_path) = attr_value(&e, b"full-path")? else {
                    bail!("container.xml rootfile missing full-path");
                };
                return normalize_zip_entry_path(&full_path);
            }
            Event::Eof => bail!("container.xml rootfile not found"),
            _ => {}
        }
        buf.clear();
    }
}

fn parse_opf(
    opf_path: &str,
    xml: &[u8],
) -> Result<(HashMap<String, EpubResource>, Vec<EpubSpineItem>)> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let opf_dir = base_dir(opf_path);
    let mut resources = HashMap::new();
    let mut spine = Vec::new();
    let mut in_manifest = false;
    let mut in_spine = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match local_name(e.name().as_ref()) {
                b"manifest" => in_manifest = true,
                b"spine" => in_spine = true,
                b"item" if in_manifest => {
                    let resource = parse_manifest_item(&opf_dir, &e)?;
                    resources.insert(resource.id.clone(), resource);
                }
                b"itemref" if in_spine => spine.push(parse_spine_item(&e)?),
                _ => {}
            },
            Event::Empty(e) => match local_name(e.name().as_ref()) {
                b"item" if in_manifest => {
                    let resource = parse_manifest_item(&opf_dir, &e)?;
                    resources.insert(resource.id.clone(), resource);
                }
                b"itemref" if in_spine => spine.push(parse_spine_item(&e)?),
                _ => {}
            },
            Event::End(e) => match local_name(e.name().as_ref()) {
                b"manifest" => in_manifest = false,
                b"spine" => in_spine = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if resources.is_empty() {
        bail!("manifest missing");
    }
    if spine.is_empty() {
        bail!("spine missing");
    }

    Ok((resources, spine))
}

fn parse_manifest_item(opf_dir: &str, e: &BytesStart<'_>) -> Result<EpubResource> {
    let id = attr_value(e, b"id")?.context("manifest item missing id")?;
    let href = attr_value(e, b"href")?.context("manifest item missing href")?;
    let media_type = attr_value(e, b"media-type")?.context("manifest item missing media-type")?;
    let properties = attr_value(e, b"properties")?;
    let full_path = resolve_epub_path(opf_dir, &href)?;

    Ok(EpubResource {
        id,
        href,
        full_path,
        media_type,
        properties,
    })
}

fn parse_spine_item(e: &BytesStart<'_>) -> Result<EpubSpineItem> {
    let idref = attr_value(e, b"idref")?.context("spine itemref missing idref")?;
    let linear = !matches!(
        attr_value(e, b"linear")?.as_deref(),
        Some("no") | Some("false") | Some("0")
    );
    Ok(EpubSpineItem { idref, linear })
}

fn extract_xhtml_image_paths(xhtml_path: &str, xml: &[u8]) -> Result<Vec<String>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let xhtml_dir = base_dir(xhtml_path);
    let mut paths = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let Some(raw_path) = extract_image_reference(&e)? else {
                    buf.clear();
                    continue;
                };
                let resolved = resolve_epub_path(&xhtml_dir, &raw_path)?;
                paths.push(resolved);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(anyhow!(err)).with_context(|| format!("parse xhtml: {xhtml_path}"))
            }
        }
        buf.clear();
    }

    Ok(paths)
}

fn extract_image_reference(e: &BytesStart<'_>) -> Result<Option<String>> {
    match local_name(e.name().as_ref()) {
        b"img" => attr_value(e, b"src"),
        b"image" => {
            if let Some(href) = attr_value_exact_or_local(e, b"href")? {
                return Ok(Some(href));
            }
            attr_value_exact(e, b"xlink:href")
        }
        _ => Ok(None),
    }
}

fn attr_value(e: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>> {
    for attr in e.attributes().with_checks(false) {
        let attr = attr?;
        if local_name(attr.key.as_ref()) == key {
            return decode_attr_value(e, &attr).map(Some);
        }
    }
    Ok(None)
}

fn attr_value_exact(e: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>> {
    for attr in e.attributes().with_checks(false) {
        let attr = attr?;
        if attr.key.as_ref() == key {
            return decode_attr_value(e, &attr).map(Some);
        }
    }
    Ok(None)
}

fn attr_value_exact_or_local(e: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>> {
    if let Some(value) = attr_value_exact(e, key)? {
        return Ok(Some(value));
    }
    attr_value(e, key)
}

fn decode_attr_value(
    e: &BytesStart<'_>,
    attr: &quick_xml::events::attributes::Attribute<'_>,
) -> Result<String> {
    let value = attr
        .decode_and_unescape_value(e.decoder())
        .context("xml attribute decode failed")?;
    Ok(value.into_owned())
}

fn find_resource_by_full_path<'a>(
    resources: &'a HashMap<String, EpubResource>,
    full_path: &str,
) -> Option<&'a EpubResource> {
    resources
        .values()
        .find(|resource| resource.full_path == full_path)
}

fn is_supported_image_path(path: &str, media_type: Option<&str>) -> bool {
    if let Some(media_type) = media_type {
        return matches!(
            media_type,
            "image/jpeg" | "image/jpg" | "image/png" | "image/gif" | "image/webp"
        );
    }

    matches!(
        path.rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp"
    )
}

fn page_image_format_hint(page: &EpubImagePage) -> Option<PageImageFormat> {
    match page.media_type.as_deref() {
        Some("image/jpeg" | "image/jpg") => Some(PageFormat::Jpeg),
        Some("image/png") => Some(PageFormat::Png),
        Some("image/gif") => Some(PageFormat::Gif),
        Some("image/webp") => Some(PageFormat::WebP),
        Some(_) => None,
        None => match page
            .image_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "jpg" | "jpeg" => Some(PageFormat::Jpeg),
            "png" => Some(PageFormat::Png),
            "gif" => Some(PageFormat::Gif),
            "webp" => Some(PageFormat::WebP),
            _ => None,
        },
    }
}

fn resolve_epub_path(base_dir: &str, raw: &str) -> Result<String> {
    let sanitized = strip_fragment_and_query(raw);
    let decoded = percent_decode_utf8(&sanitized)?;
    if decoded.is_empty() {
        bail!("empty path");
    }
    if decoded.starts_with('/') {
        return normalize_zip_entry_path(&decoded);
    }
    if base_dir.is_empty() {
        return normalize_zip_entry_path(&decoded);
    }
    normalize_zip_entry_path(&format!("{base_dir}/{decoded}"))
}

fn normalize_zip_entry_path(path: &str) -> Result<String> {
    let normalized = path.replace('\\', "/");
    let mut stack = Vec::new();

    for segment in normalized.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    bail!("path escapes root: {path}");
                }
            }
            _ => stack.push(segment),
        }
    }

    if stack.is_empty() {
        bail!("path resolves to empty: {path}");
    }

    Ok(stack.join("/"))
}

fn base_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

fn strip_fragment_and_query(path: &str) -> String {
    let end = path.find(['#', '?']).unwrap_or(path.len());
    path[..end].to_string()
}

fn percent_decode_utf8(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                bail!("invalid percent encoding: {value}");
            }
            let hi = decode_hex(bytes[index + 1])?;
            let lo = decode_hex(bytes[index + 2])?;
            out.push((hi << 4) | lo);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(out).context("invalid utf-8 in path")
}

fn decode_hex(ch: u8) -> Result<u8> {
    match ch {
        b'0'..=b'9' => Ok(ch - b'0'),
        b'a'..=b'f' => Ok(ch - b'a' + 10),
        b'A'..=b'F' => Ok(ch - b'A' + 10),
        _ => bail!("invalid hex digit"),
    }
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}
