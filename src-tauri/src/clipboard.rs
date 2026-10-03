use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::OnceLock;

const CLIPBOARD_MAX_TEXT_BYTES: usize = 256 * 1024;
// Raw RGBA can be large (a 2560x1440 frame is ~14 MB); cap it so a stray huge
// copy never floods the LAN transport. Images above this are skipped.
const CLIPBOARD_MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
// File-clipboard budget: the total decoded size of all files in one copy.
// The whole payload rides one 48 MB QUIC stream and base64 inflates ~4/3, so
// anything larger cannot cross anyway — skipping keeps behavior predictable.
const CLIPBOARD_MAX_FILES_BYTES: usize = 24 * 1024 * 1024;

/// One file of a file-clipboard copy: decoded content in memory. The wire
/// format base64-encodes `data`; `name` is the file name only (no path) —
/// the receiver lands files under its own transfer directory.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClipboardFile {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClipboardImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    // Raw RGBA pixels for the legacy "imageRgba" wire format. Empty when this
    // struct carries a PNG-encoded payload ("imagePng") instead.
    pub(crate) rgba_base64: String,
    // PNG-encoded pixels (base64). A 2560x1440 screenshot compresses to a few
    // hundred KB here versus ~14 MB of raw RGBA, so senders prefer this format
    // whenever it encodes and is actually smaller.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) png_base64: String,
}

/// One unit of clipboard content read from (or written to) the local system.
#[derive(Debug, Clone)]
pub(crate) enum ClipboardContent {
    Text(String),
    Image(ClipboardImage),
    Files(Vec<ClipboardFile>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardContentHint {
    Image,
    Text,
    Unknown,
}

fn clipboard_signature_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

impl ClipboardContent {
    pub(crate) fn is_oversized(&self) -> bool {
        match self {
            ClipboardContent::Text(text) => text.len() > CLIPBOARD_MAX_TEXT_BYTES,
            ClipboardContent::Files(files) => {
                files.iter().map(|file| file.data.len()).sum::<usize>() > CLIPBOARD_MAX_FILES_BYTES
            }
            ClipboardContent::Image(image) => {
                if !image.png_base64.is_empty() {
                    // Bound the PNG payload itself so a decompression bomb
                    // cannot be handed to the decoder below; `decode_png`
                    // separately rejects decoded RGBA above the same budget.
                    return image.png_base64.len() / 4 * 3 > CLIPBOARD_MAX_IMAGE_BYTES;
                }
                // base64 inflates ~4/3; compare against the decoded RGBA budget.
                let padding = image
                    .rgba_base64
                    .bytes()
                    .rev()
                    .take(2)
                    .take_while(|byte| *byte == b'=')
                    .count();
                (image.rgba_base64.len() / 4 * 3).saturating_sub(padding)
                    > CLIPBOARD_MAX_IMAGE_BYTES
            }
        }
    }

    /// A stable fingerprint used to detect "did the clipboard change" and to
    /// suppress echoing content we just received from a peer.
    pub(crate) fn signature(&self) -> String {
        match self {
            ClipboardContent::Text(text) => format!("text:{text}"),
            ClipboardContent::Files(files) => {
                // Path-free signature: file NAME + content length per entry,
                // sorted so the same copy hashes the same on every machine.
                // Cheap to recompute per poll — file CONTENTS are not hashed.
                let mut parts: Vec<String> = files
                    .iter()
                    .map(|file| format!("{}:{}", file.name, file.data.len()))
                    .collect();
                parts.sort();
                format!("files:{}:{:016x}", parts.len(), clipboard_signature_hash(parts.join("|").as_bytes()))
            }
            ClipboardContent::Image(image) => {
                // Hash both representations so the same pixels produce the same
                // signature whether they travel as raw RGBA or PNG. The PNG
                // field is empty on legacy RGBA flows, so existing signatures
                // are unchanged.
                let mut bytes = image.rgba_base64.as_bytes().to_vec();
                bytes.extend_from_slice(image.png_base64.as_bytes());
                format!(
                    "image:{}x{}:{}:{:016x}",
                    image.width,
                    image.height,
                    bytes.len(),
                    clipboard_signature_hash(&bytes)
                )
            }
        }
    }
}

pub(crate) fn read_text() -> Result<String, String> {
    read_system_text()
}

pub(crate) fn write_text(text: &str) -> Result<(), String> {
    write_system_text(text)
}

// File-clipboard landing directory, wired once at app startup (Downloads\MyKVM
// Transfers\Clipboard, with an app-data fallback). Received files are written
// here and the local clipboard is pointed at them, so Ctrl+V pastes directly.
static FILES_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

pub(crate) fn set_files_dir(dir: std::path::PathBuf) {
    let _ = FILES_DIR.set(dir);
}

fn files_dir() -> Option<std::path::PathBuf> {
    FILES_DIR.get().cloned()
}

/// Reads copied files (Explorer Ctrl+C) from the clipboard via arboard's
/// file-list support. Copies containing directories, unreadable files, or more
/// than the total budget are skipped entirely — a partial file copy would be
/// confusing on the far side.
fn read_files() -> Option<Vec<ClipboardFile>> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let paths = clipboard.get().file_list().ok()?;
    if paths.is_empty() {
        return None;
    }

    let mut files = Vec::with_capacity(paths.len());
    let mut total = 0_usize;
    for path in &paths {
        let Ok(meta) = fs::metadata(path) else {
            return None;
        };
        if !meta.is_file() {
            // A folder (or a mixed/unreadable selection) — not a file copy we
            // can transfer; fall back to text/image handling.
            return None;
        }
        total += meta.len() as usize;
        if total > CLIPBOARD_MAX_FILES_BYTES {
            log::info!(
                "file-clipboard copy skipped: total size {total} exceeds budget"
            );
            return None;
        }
        let Some(name) = path.file_name() else {
            return None;
        };
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(error) => {
                log::info!("file-clipboard copy skipped: reading {} failed: {error}", path.display());
                return None;
            }
        };
        files.push(ClipboardFile {
            name: name.to_string_lossy().to_string(),
            data,
        });
    }

    (!files.is_empty()).then_some(files)
}

/// Writes a received file-clipboard payload into the landing directory and
/// points the local clipboard at the new files, so Ctrl+V pastes them. File
/// names are preserved (light sanitization only); an existing file of the same
/// name is overwritten so re-syncs stay name-stable (the echo suppression keys
/// off name+size).
fn write_files(files: &[ClipboardFile]) -> Result<(), String> {
    let Some(dir) = files_dir() else {
        return Err("file-clipboard landing directory is not configured".into());
    };
    fs::create_dir_all(&dir).map_err(|error| format!("failed to create {}: {error}", dir.display()))?;

    let mut paths = Vec::with_capacity(files.len());
    for file in files {
        let name = sanitize_file_clipboard_name(&file.name)
            .ok_or_else(|| format!("invalid file name {:?}", file.name))?;
        let path = dir.join(&name);
        fs::write(&path, &file.data)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        paths.push(path);
    }

    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("failed to open clipboard: {error}"))?;
    clipboard
        .set()
        .file_list(paths.as_slice())
        .map_err(|error| format!("failed to place files on the clipboard: {error}"))?;
    log::info!(
        "file-clipboard: wrote {} file(s) to {} and pointed the local clipboard at them",
        files.len(),
        dir.display()
    );
    Ok(())
}

/// Light name sanitization for received file-clipboard entries: path
/// separators become underscores and reserved names are rejected.
fn sanitize_file_clipboard_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '\\' | '/' => '_',
            character => character,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn write_content(content: &ClipboardContent) -> Result<(), String> {
    // arboard's macOS image write is not wrapped in an autorelease pool. Synced
    // clipboards are written on long-lived QUIC worker threads, which have no
    // pool, so each received image's NSImage/TIFF temporaries (the whole image)
    // were never freed: a few dozen screenshots reached ~2 GB (discussion #32).
    #[cfg(target_os = "macos")]
    let _pool = crate::input::macos_appkit::autorelease_pool();
    match content {
        ClipboardContent::Text(text) => write_text(text),
        ClipboardContent::Image(image) => write_image(image),
        ClipboardContent::Files(files) => write_files(files),
    }
}

/// Reads whatever is currently on the clipboard. The shared policy lives here:
/// when the platform can identify a current image format, wait for an image
/// read instead of falling back to stale text from a previous clipboard format.
pub(crate) fn read_content() -> Option<ClipboardContent> {
    // Same reason as `write_content`; arboard pools its image read but not
    // `Clipboard::new()`, and this runs on the pool-less clipboard thread.
    #[cfg(target_os = "macos")]
    let _pool = crate::input::macos_appkit::autorelease_pool();
    // Copied FILES win first: Explorer copies carry text/image formats too,
    // and without this priority the file copy would sync as stale text.
    if let Some(files) = read_files() {
        return Some(ClipboardContent::Files(files));
    }
    read_content_for_hint(content_hint(), read_text_content, read_image_content)
}

pub(crate) fn change_count() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        crate::input::macos_appkit::clipboard_change_count()
    }
    #[cfg(target_os = "windows")]
    {
        let sequence =
            unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
        (sequence != 0).then_some(u64::from(sequence))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

fn read_content_for_hint<F, G>(
    hint: ClipboardContentHint,
    mut read_text: F,
    mut read_image: G,
) -> Option<ClipboardContent>
where
    F: FnMut() -> Option<ClipboardContent>,
    G: FnMut() -> Option<ClipboardContent>,
{
    match hint {
        ClipboardContentHint::Image => read_image(),
        ClipboardContentHint::Text => read_text(),
        ClipboardContentHint::Unknown => read_unknown_content(read_text, read_image),
    }
}

fn read_text_content() -> Option<ClipboardContent> {
    read_text()
        .ok()
        .filter(|text| !text.is_empty())
        .map(ClipboardContent::Text)
}

fn read_image_content() -> Option<ClipboardContent> {
    read_image().map(ClipboardContent::Image)
}

#[cfg(target_os = "windows")]
fn read_unknown_content<F, G>(read_text: F, mut read_image: G) -> Option<ClipboardContent>
where
    F: FnMut() -> Option<ClipboardContent>,
    G: FnMut() -> Option<ClipboardContent>,
{
    read_image().or_else(read_text)
}

#[cfg(not(target_os = "windows"))]
fn read_unknown_content<F, G>(mut read_text: F, read_image: G) -> Option<ClipboardContent>
where
    F: FnMut() -> Option<ClipboardContent>,
    G: FnMut() -> Option<ClipboardContent>,
{
    read_text().or_else(read_image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_budget_accounts_for_base64_padding() {
        let encoded_len = CLIPBOARD_MAX_IMAGE_BYTES.div_ceil(3) * 4;
        let mut image = ClipboardImage {
            width: 8192,
            height: 1024,
            rgba_base64: "A".repeat(encoded_len - 1) + "=",
            png_base64: String::new(),
        };
        assert!(!ClipboardContent::Image(image.clone()).is_oversized());
        image.rgba_base64.replace_range(encoded_len - 1.., "A");
        assert!(ClipboardContent::Image(image).is_oversized());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unknown_clipboard_prefers_text_before_image() {
        let content = read_content_for_hint(
            ClipboardContentHint::Unknown,
            || Some(ClipboardContent::Text("中文测试 abc 123".into())),
            || {
                Some(ClipboardContent::Image(ClipboardImage {
                    width: 1,
                    height: 1,
                    rgba_base64: "AAAAAA==".into(),
                    png_base64: String::new(),
                }))
            },
        );

        match content {
            Some(ClipboardContent::Text(text)) => assert_eq!(text, "中文测试 abc 123"),
            _ => panic!("expected text to win when the platform cannot identify clipboard format"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn unknown_clipboard_keeps_windows_image_first_fallback() {
        let content = read_content_for_hint(
            ClipboardContentHint::Unknown,
            || Some(ClipboardContent::Text("中文测试 abc 123".into())),
            || {
                Some(ClipboardContent::Image(ClipboardImage {
                    width: 1,
                    height: 1,
                    rgba_base64: "AAAAAA==".into(),
                    png_base64: String::new(),
                }))
            },
        );

        match content {
            Some(ClipboardContent::Image(image)) => assert_eq!(image.width, 1),
            _ => panic!("expected Windows fallback to keep image priority"),
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn utf8_command_sets_locale_for_clipboard_tools() {
        let command = utf8_command("pbpaste");
        let envs: std::collections::HashMap<_, _> = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_str()?, value?.to_str()?)))
            .collect();

        assert_eq!(envs.get("LANG"), Some(&"en_US.UTF-8"));
        assert_eq!(envs.get("LC_CTYPE"), Some(&"en_US.UTF-8"));
    }

    fn sample_rgba_image() -> ClipboardImage {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

        // 2x2 pixels: red, green, blue, white.
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255, //
            0, 255, 0, 255, //
            0, 0, 255, 255, //
            255, 255, 255, 255,
        ];
        ClipboardImage {
            width: 2,
            height: 2,
            rgba_base64: BASE64.encode(rgba),
            png_base64: String::new(),
        }
    }

    #[test]
    fn png_round_trip_preserves_pixels_and_signature() {
        let original = sample_rgba_image();
        let png_base64 = encode_png(&original).expect("png encode");
        assert!(
            !png_base64.is_empty(),
            "even a tiny image should produce a PNG payload"
        );

        let decoded = decode_png(&png_base64, original.width, original.height)
            .expect("png decode");
        assert_eq!(decoded.width, original.width);
        assert_eq!(decoded.height, original.height);
        assert_eq!(decoded.rgba_base64, original.rgba_base64);
        assert!(decoded.png_base64.is_empty());
        assert_eq!(
            ClipboardContent::Image(original).signature(),
            ClipboardContent::Image(decoded).signature(),
            "the canonical RGBA form must keep the same signature across the wire format"
        );
    }

    #[test]
    fn png_decode_rejects_dimension_mismatch_and_garbage() {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

        let original = sample_rgba_image();
        let png_base64 = encode_png(&original).expect("png encode");

        assert!(decode_png(&png_base64, 3, 2).is_none(), "width mismatch");
        assert!(decode_png(&png_base64, 2, 3).is_none(), "height mismatch");
        assert!(
            decode_png(
                &BASE64.encode(b"not a png"),
                2,
                2
            )
            .is_none(),
            "garbage payload"
        );
        assert!(decode_png("", 2, 2).is_none(), "empty payload");
    }

    #[test]
    fn png_encode_rejects_rgba_payloads_and_empty_images() {
        let mut image = sample_rgba_image();
        image.rgba_base64 = String::new();
        assert!(encode_png(&image).is_none(), "empty RGBA source");

        let mut image = sample_rgba_image();
        image.png_base64 = "already-encoded".into();
        assert!(encode_png(&image).is_none(), "already a PNG payload");

        let mut image = sample_rgba_image();
        image.rgba_base64 = "AAAA".into();
        assert!(encode_png(&image).is_none(), "RGBA length must match w*h*4");
    }

    #[test]
    fn png_budget_bounds_the_encoded_payload() {
        let mut image = sample_rgba_image();
        // Simulate an over-budget encoded payload; the budget applies to the
        // PNG bytes themselves so a decompression bomb is rejected pre-decode.
        image.png_base64 = "A".repeat(CLIPBOARD_MAX_IMAGE_BYTES / 3 * 4 + 4);
        assert!(ClipboardContent::Image(image).is_oversized());
    }

    fn sample_files() -> Vec<ClipboardFile> {
        vec![
            ClipboardFile { name: "a.txt".into(), data: b"hello".to_vec() },
            ClipboardFile { name: "b.txt".into(), data: b"world".to_vec() },
        ]
    }

    #[test]
    fn files_signature_is_stable_and_order_free_and_content_aware() {
        let files = sample_files();
        let signature = ClipboardContent::Files(files.clone()).signature();

        // Same content in the same order → same signature (echo suppression).
        assert_eq!(signature, ClipboardContent::Files(files.clone()).signature());
        // Different size → different signature. (A same-size content edit is
        // deliberately not re-synced: the signature is name+size so polls
        // never re-hash file contents.)
        let mut changed = files.clone();
        changed[0].data = b"hellp!".to_vec();
        assert_ne!(signature, ClipboardContent::Files(changed).signature());
        // Reordered entries are the same copy: the signature sorts name+size
        // pairs, so Explorer's arbitrary selection order doesn't matter.
        let reordered = vec![files[1].clone(), files[0].clone()];
        assert_eq!(
            signature,
            ClipboardContent::Files(reordered).signature()
        );
    }

    #[test]
    fn files_budget_rejects_oversized_copies() {
        let mut files = sample_files();
        files[0].data = vec![0_u8; CLIPBOARD_MAX_FILES_BYTES];
        assert!(ClipboardContent::Files(files).is_oversized());
        // Just inside the budget is fine.
        let mut files = sample_files();
        files[0].data = vec![0_u8; CLIPBOARD_MAX_FILES_BYTES - 8];
        assert!(!ClipboardContent::Files(files).is_oversized());
    }

    #[test]
    fn file_clipboard_name_sanitizes_path_tricks() {
        assert_eq!(
            sanitize_file_clipboard_name("notes\\..\\..\\evil.txt").as_deref(),
            Some("notes_.._.._evil.txt")
        );
        assert_eq!(sanitize_file_clipboard_name(".."), None);
        assert_eq!(sanitize_file_clipboard_name("  "), None);
    }
}

fn read_image() -> Option<ClipboardImage> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    let arboard_image = arboard::Clipboard::new().ok().and_then(|mut clipboard| {
        let image = clipboard.get_image().ok()?;
        if image.width == 0 || image.height == 0 || image.bytes.is_empty() {
            return None;
        }
        if image.bytes.len() > CLIPBOARD_MAX_IMAGE_BYTES {
            return None;
        }

        Some(ClipboardImage {
            width: image.width as u32,
            height: image.height as u32,
            rgba_base64: BASE64.encode(image.bytes.as_ref()),
            png_base64: String::new(),
        })
    });

    arboard_image.or_else(|| {
        #[cfg(target_os = "windows")]
        {
            read_windows_dib_image()
        }

        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    })
}

fn write_image(image: &ClipboardImage) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    let bytes = BASE64
        .decode(image.rgba_base64.as_bytes())
        .map_err(|error| format!("failed to decode clipboard image: {error}"))?;
    let width = image.width as usize;
    let height = image.height as usize;
    if width == 0
        || height == 0
        || bytes.len() > CLIPBOARD_MAX_IMAGE_BYTES
        || bytes.len() != width.saturating_mul(height).saturating_mul(4)
    {
        return Err("clipboard image has invalid dimensions".into());
    }

    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("failed to open clipboard: {error}"))?;
    clipboard
        .set_image(arboard::ImageData {
            width,
            height,
            bytes: std::borrow::Cow::Owned(bytes),
        })
        .map_err(|error| format!("failed to write clipboard image: {error}"))
}

#[cfg(target_os = "windows")]
fn content_hint() -> ClipboardContentHint {
    use windows_sys::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Ole::{CF_BITMAP, CF_DIB, CF_DIBV5, CF_UNICODETEXT};

    let png_format = unsafe { RegisterClipboardFormatW(crate::wide_null("PNG").as_ptr()) };
    let image_formats = [
        png_format,
        u32::from(CF_DIBV5),
        u32::from(CF_DIB),
        u32::from(CF_BITMAP),
    ];
    if image_formats
        .iter()
        .any(|format| *format != 0 && unsafe { IsClipboardFormatAvailable(*format) } != 0)
    {
        return ClipboardContentHint::Image;
    }
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) } != 0 {
        ClipboardContentHint::Text
    } else {
        ClipboardContentHint::Unknown
    }
}

#[cfg(not(target_os = "windows"))]
fn content_hint() -> ClipboardContentHint {
    ClipboardContentHint::Unknown
}

#[cfg(target_os = "windows")]
fn read_windows_dib_image() -> Option<ClipboardImage> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5};

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return None;
    }
    let _guard = ClipboardGuard;

    for format in [u32::from(CF_DIBV5), u32::from(CF_DIB)] {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            continue;
        }
        let len = unsafe { GlobalSize(handle) };
        if len == 0 || len > CLIPBOARD_MAX_IMAGE_BYTES.saturating_add(256) {
            continue;
        }
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            continue;
        }
        let data = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        let decoded = decode_windows_dib_image(data);
        unsafe {
            let _ = GlobalUnlock(handle);
        }
        if decoded.is_some() {
            return decoded;
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn decode_windows_dib_image(data: &[u8]) -> Option<ClipboardImage> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use image::{codecs::bmp::BmpDecoder, DynamicImage, ImageDecoder};

    let decoder = BmpDecoder::new_without_file_header(std::io::Cursor::new(data)).ok()?;
    let (width, height) = decoder.dimensions();
    let rgba = DynamicImage::from_decoder(decoder).ok()?.into_rgba8();
    let bytes = rgba.into_raw();
    if width == 0 || height == 0 || bytes.is_empty() || bytes.len() > CLIPBOARD_MAX_IMAGE_BYTES {
        return None;
    }

    Some(ClipboardImage {
        width,
        height,
        rgba_base64: BASE64.encode(bytes),
        png_base64: String::new(),
    })
}

/// PNG-encode the image's RGBA pixels for the wire. Returns base64 of the PNG,
/// or `None` when encoding fails (senders then fall back to the legacy raw
/// RGBA format). Callers compare sizes and only prefer PNG when it is smaller.
pub(crate) fn encode_png(image: &ClipboardImage) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

    if !image.png_base64.is_empty() || image.rgba_base64.is_empty() {
        return None;
    }
    let rgba = BASE64
        .decode(image.rgba_base64.as_bytes())
        .ok()?;
    if image.width == 0
        || image.height == 0
        || rgba.len() != image.width as usize * image.height as usize * 4
    {
        return None;
    }

    let mut png: Vec<u8> = Vec::new();
    PngEncoder::new(std::io::Cursor::new(&mut png))
        .write_image(&rgba, image.width, image.height, ExtendedColorType::Rgba8)
        .ok()?;
    Some(BASE64.encode(png))
}

/// Decode a wire "imagePng" payload back into the canonical RGBA form. The
/// declared dimensions must match the PNG header, and the decoded RGBA must
/// stay inside the image budget, so a decompression bomb cannot inflate
/// unbounded.
pub(crate) fn decode_png(png_base64: &str, width: u32, height: u32) -> Option<ClipboardImage> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use image::ImageReader;

    let png = BASE64.decode(png_base64.as_bytes()).ok()?;
    if png.is_empty() || width == 0 || height == 0 {
        return None;
    }

    // Read the header first and bound the allocation before decoding.
    let (actual_width, actual_height) = ImageReader::new(std::io::Cursor::new(&png))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if actual_width != width || actual_height != height {
        return None;
    }
    let decoded_bytes = u64::from(width) * u64::from(height) * 4;
    if decoded_bytes > CLIPBOARD_MAX_IMAGE_BYTES as u64 {
        return None;
    }

    let rgba = ImageReader::new(std::io::Cursor::new(&png))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .into_rgba8();
    if rgba.width() != width || rgba.height() != height {
        return None;
    }

    Some(ClipboardImage {
        width,
        height,
        rgba_base64: BASE64.encode(rgba.into_raw()),
        png_base64: String::new(),
    })
}

#[cfg(target_os = "windows")]
fn read_system_text() -> Result<String, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("failed to open clipboard: {error}"))?;
    clipboard
        .get_text()
        .map_err(|error| format!("failed to read clipboard text: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn read_system_text() -> Result<String, String> {
    use std::process::Command;

    let output = if cfg!(target_os = "macos") {
        utf8_command("pbpaste").output()
    } else {
        Command::new("sh")
            .args([
                "-c",
                "wl-paste -n 2>/dev/null || xclip -selection clipboard -out",
            ])
            .output()
    }
    .map_err(|error| format!("failed to read clipboard: {error}"))?;

    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("clipboard text is not valid UTF-8: {error}"))
    } else {
        Err(format!(
            "clipboard command exited with status {}",
            output.status
        ))
    }
}

#[cfg(target_os = "windows")]
fn write_system_text(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("failed to open clipboard: {error}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|error| format!("failed to write clipboard text: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn write_system_text(text: &str) -> Result<(), String> {
    use std::{io::Write, process::Command, process::Stdio};

    let mut child = if cfg!(target_os = "macos") {
        utf8_command("pbcopy").stdin(Stdio::piped()).spawn()
    } else {
        Command::new("sh")
            .args(["-c", "wl-copy 2>/dev/null || xclip -selection clipboard"])
            .stdin(Stdio::piped())
            .spawn()
    }
    .map_err(|error| format!("failed to write clipboard: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("failed to send clipboard text: {error}"))?;
    }

    let status = child
        .wait()
        .map_err(|error| format!("failed to finish clipboard write: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("clipboard command exited with status {status}"))
    }
}

#[cfg(not(target_os = "windows"))]
fn utf8_command(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .env("LANG", "en_US.UTF-8")
        .env("LC_CTYPE", "en_US.UTF-8");
    command
}
