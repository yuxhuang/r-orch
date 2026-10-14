//! Clipboard access for TUI image paste — replaces
//! `src/infrastructure/clipboard-service.ts`.
//!
//! The TS service shells out to osascript / xclip / PowerShell; here `arboard`
//! does the platform work (ARCHITECTURE §4.8). The TUI is L2, so the mechanism
//! may differ, but the contract is kept: **no failure ever escapes**. An
//! unusable clipboard reads as [`ClipboardContentType::Empty`] / `None`, and
//! every OS call is bounded by the same 3 s as the TS `EXEC_TIMEOUT_MS`.

use std::time::Duration;

use arboard::Clipboard;

const EXEC_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardContentType {
    Image,
    Text,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    /// PNG-encoded.
    pub data: Vec<u8>,
    pub ext: &'static str,
}

/// RGBA8 pixels, row-major.
#[derive(Debug, Clone)]
pub struct RawImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// The OS clipboard, as narrowly as the service needs it. Errors are erased
/// because every caller degrades the same way.
pub trait ClipboardBackend: Send + Sync + 'static {
    fn image(&self) -> Option<RawImage>;
    fn text(&self) -> Option<String>;
}

/// `arboard`-backed clipboard. A handle is opened per call: reads are
/// one-shot and a stale handle is the usual cause of X11/Wayland failures.
pub struct SystemClipboard;

impl ClipboardBackend for SystemClipboard {
    fn image(&self) -> Option<RawImage> {
        let img = Clipboard::new().ok()?.get_image().ok()?;
        Some(RawImage {
            width: img.width,
            height: img.height,
            rgba: img.bytes.into_owned(),
        })
    }

    fn text(&self) -> Option<String> {
        Clipboard::new().ok()?.get_text().ok()
    }
}

/// Whether a clipboard can be opened at all (TS `isClipboardToolAvailable`).
pub fn is_clipboard_available() -> bool {
    Clipboard::new().is_ok()
}

fn classify(backend: &dyn ClipboardBackend) -> ClipboardContentType {
    if backend.image().is_some_and(|i| i.width > 0 && i.height > 0) {
        return ClipboardContentType::Image;
    }
    match backend.text() {
        Some(text) if !text.trim().is_empty() => ClipboardContentType::Text,
        _ => ClipboardContentType::Empty,
    }
}

fn encode_png(image: &RawImage) -> Option<Vec<u8>> {
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    if width == 0 || height == 0 || image.rgba.len() != image.width * image.height * 4 {
        return None;
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&image.rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

fn read_image(backend: &dyn ClipboardBackend) -> Option<ClipboardImage> {
    let data = encode_png(&backend.image()?)?;
    Some(ClipboardImage { data, ext: "png" })
}

/// Run a blocking clipboard call with the TS 3 s bound. A hung call keeps its
/// thread but the caller gets the fallback.
async fn bounded<T: Send + 'static>(fallback: T, job: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::time::timeout(EXEC_TIMEOUT, tokio::task::spawn_blocking(job)).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            tracing::warn!(%error, "clipboard task failed");
            fallback
        }
        Err(_) => {
            tracing::warn!("clipboard read timed out");
            fallback
        }
    }
}

pub async fn detect_clipboard_type_with<B: ClipboardBackend>(backend: B) -> ClipboardContentType {
    bounded(ClipboardContentType::Empty, move || classify(&backend)).await
}

pub async fn get_clipboard_image_with<B: ClipboardBackend>(backend: B) -> Option<ClipboardImage> {
    bounded(None, move || {
        if classify(&backend) != ClipboardContentType::Image {
            return None;
        }
        read_image(&backend)
    })
    .await
}

/// `image`, `text` or `empty` — never an error.
pub async fn detect_clipboard_type() -> ClipboardContentType {
    detect_clipboard_type_with(SystemClipboard).await
}

/// The clipboard image as PNG, or `None` when it holds no image or cannot be read.
pub async fn get_clipboard_image() -> Option<ClipboardImage> {
    get_clipboard_image_with(SystemClipboard).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct Fake {
        image: Option<(usize, usize, Vec<u8>)>,
        text: Option<String>,
    }

    impl ClipboardBackend for Fake {
        fn image(&self) -> Option<RawImage> {
            self.image.clone().map(|(width, height, rgba)| RawImage {
                width,
                height,
                rgba,
            })
        }
        fn text(&self) -> Option<String> {
            self.text.clone()
        }
    }

    fn pixel() -> Option<(usize, usize, Vec<u8>)> {
        Some((1, 1, vec![255, 0, 0, 255]))
    }

    #[tokio::test]
    async fn an_image_wins_over_text() {
        let fake = Fake {
            image: pixel(),
            text: Some("hi".into()),
        };
        assert_eq!(
            detect_clipboard_type_with(fake).await,
            ClipboardContentType::Image
        );
    }

    #[tokio::test]
    async fn text_is_text_and_blank_text_is_empty() {
        let text = Fake {
            text: Some("hello".into()),
            ..Default::default()
        };
        assert_eq!(
            detect_clipboard_type_with(text).await,
            ClipboardContentType::Text
        );
        let blank = Fake {
            text: Some("  \n".into()),
            ..Default::default()
        };
        assert_eq!(
            detect_clipboard_type_with(blank).await,
            ClipboardContentType::Empty
        );
    }

    #[tokio::test]
    async fn an_unreadable_clipboard_is_empty() {
        assert_eq!(
            detect_clipboard_type_with(Fake::default()).await,
            ClipboardContentType::Empty
        );
        assert!(get_clipboard_image_with(Fake::default()).await.is_none());
    }

    #[tokio::test]
    async fn a_panicking_backend_degrades_instead_of_propagating() {
        struct Boom;
        impl ClipboardBackend for Boom {
            fn image(&self) -> Option<RawImage> {
                panic!("clipboard exploded")
            }
            fn text(&self) -> Option<String> {
                None
            }
        }
        assert_eq!(
            detect_clipboard_type_with(Boom).await,
            ClipboardContentType::Empty
        );
        assert!(get_clipboard_image_with(Boom).await.is_none());
    }

    #[tokio::test]
    async fn image_is_returned_as_a_decodable_png() {
        let fake = Fake {
            image: Some((2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])),
            text: None,
        };
        let img = get_clipboard_image_with(fake).await.unwrap();
        assert_eq!(img.ext, "png");
        assert_eq!(&img.data[..8], b"\x89PNG\r\n\x1a\n");
        let mut reader = png::Decoder::new(std::io::Cursor::new(&img.data))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&buf[..8], [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[tokio::test]
    async fn text_only_clipboard_has_no_image() {
        let fake = Fake {
            text: Some("hello".into()),
            ..Default::default()
        };
        assert!(get_clipboard_image_with(fake).await.is_none());
    }

    #[tokio::test]
    async fn malformed_pixel_data_is_no_image() {
        let fake = Fake {
            image: Some((2, 2, vec![0; 3])),
            text: None,
        };
        assert!(get_clipboard_image_with(fake).await.is_none());
    }

    #[tokio::test]
    async fn the_system_clipboard_degrades_rather_than_erroring_when_headless() {
        // This used to bind every result to `_`, which asserts nothing at all
        // (code m6): the calls could have returned anything, or panicked only
        // on some machines, and the test would still have passed. The real
        // contract is that each one *degrades* — no clipboard is a `None`/
        // `false`, never an error and never a panic.
        //
        // CI and this sandbox are headless, so that is the branch under test;
        // a developer running it on a desktop may legitimately see a
        // clipboard, which is why the assertions name both outcomes rather
        // than pinning one.
        let kind = detect_clipboard_type().await;
        let image = get_clipboard_image().await;
        let available = is_clipboard_available();

        if available {
            // With a clipboard present, the type is whatever is on it and the
            // image is `Some` only if it really holds an image.
            assert!(
                image.is_none() || kind == ClipboardContentType::Image,
                "an image was returned but the type says {kind:?}"
            );
        } else {
            assert_eq!(
                kind,
                ClipboardContentType::Empty,
                "no clipboard must read as Empty, not as text or an image"
            );
            assert!(image.is_none(), "no clipboard cannot yield an image");
        }
    }
}
