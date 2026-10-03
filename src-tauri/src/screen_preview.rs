//! On-demand screen preview for the layout canvas / device list.
//!
//! The peer captures its screen (GDI BitBlt on Windows), scales it to a small
//! JPEG and answers the preview request over the existing QUIC stream path —
//! the reply is `ok:<base64 jpeg>` riding the same `StreamHandler -> String`
//! channel as every other ack. One shot per request (the frontend fetches on
//! demand), so bandwidth is bounded by user attention; there is no background
//! stream.
#![allow(dead_code)] // macOS/Linux stubs are compiled out per-platform below.

use base64::Engine;

pub(crate) const PREVIEW_PROTOCOL: &str = "mykvm.preview.v1";
/// Longest edge of the returned thumbnail. ~480px wide JPEG quality 60 lands
/// at roughly 30-80 KB per frame.
pub(crate) const PREVIEW_MAX_WIDTH: u32 = 480;
const JPEG_QUALITY: u8 = 60;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewRequestPacket {
    pub(crate) protocol: String,
    pub(crate) origin_id: String,
    pub(crate) target_id: String,
    pub(crate) cluster_id: String,
    pub(crate) pair_secret: String,
    pub(crate) max_width: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewImage {
    pub(crate) base64: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) fn preview_request_packet(
    origin_id: &str,
    target_id: &str,
    cluster_id: &str,
    pair_secret: &str,
) -> PreviewRequestPacket {
    PreviewRequestPacket {
        protocol: PREVIEW_PROTOCOL.into(),
        origin_id: origin_id.into(),
        target_id: target_id.into(),
        cluster_id: cluster_id.into(),
        pair_secret: pair_secret.into(),
        max_width: PREVIEW_MAX_WIDTH,
    }
}

/// Capture the whole virtual screen and encode it as a JPEG thumbnail
/// (base64, plus the final pixel size). Any failure is an Err — the caller
/// surfaces it as a rejected request.
#[cfg(target_os = "windows")]
pub(crate) fn capture_jpeg_base64(max_width: u32) -> Result<PreviewImage, String> {
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
        GetWindowDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetDesktopWindow, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    unsafe {
        let desktop = GetDesktopWindow();
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if width <= 0 || height <= 0 {
            return Err("虚拟屏幕尺寸无效".into());
        }

        let screen_dc = GetWindowDC(desktop);
        if screen_dc.is_null() {
            return Err("GetWindowDC 失败".into());
        }
        let memory_dc = CreateCompatibleDC(screen_dc);
        if memory_dc.is_null() {
            ReleaseDC(desktop, screen_dc);
            return Err("CreateCompatibleDC 失败".into());
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.is_null() {
            DeleteDC(memory_dc);
            ReleaseDC(desktop, screen_dc);
            return Err("CreateCompatibleBitmap 失败".into());
        }
        let previous = SelectObject(memory_dc, bitmap);
        let blit = BitBlt(
            memory_dc,
            0,
            0,
            width,
            height,
            screen_dc,
            x,
            y,
            SRCCOPY,
        );
        // Read the bits back before any early return so cleanup always runs.
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0_u8; (width as usize) * (height as usize) * 4];
        let lines = GetDIBits(
            memory_dc,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(memory_dc, previous);
        DeleteObject(bitmap);
        DeleteDC(memory_dc);
        ReleaseDC(desktop, screen_dc);

        if blit == 0 || lines == 0 {
            return Err("屏幕像素读取失败".into());
        }

        // GDI gives BGRA; the image crate wants RGBA.
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        encode_jpeg_base64(&pixels, width as u32, height as u32, max_width)
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn capture_jpeg_base64(_max_width: u32) -> Result<PreviewImage, String> {
    Err("屏幕预览暂不支持当前平台。".into())
}

fn encode_jpeg_base64(
    bgra_swapped_rgba: &[u8],
    width: u32,
    height: u32,
    max_width: u32,
) -> Result<PreviewImage, String> {
    let image = image::RgbaImage::from_raw(width, height, bgra_swapped_rgba.to_vec())
        .ok_or_else(|| "屏幕像素缓冲与尺寸不符".to_string())?;
    let scaled = if width > max_width {
        let ratio = f64::from(max_width) / f64::from(width);
        let target_height = ((f64::from(height) * ratio).round() as u32).max(1);
        image::imageops::thumbnail(&image, max_width, target_height)
    } else {
        image
    };
    let mut jpeg = Vec::new();
    {
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY);
        encoder
            .encode(
                scaled.as_raw(),
                scaled.width(),
                scaled.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| format!("JPEG 编码失败: {error}"))?;
    }
    Ok(PreviewImage {
        base64: base64::engine::general_purpose::STANDARD.encode(&jpeg),
        width: scaled.width(),
        height: scaled.height(),
    })
}
