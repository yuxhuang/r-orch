//! Clipboard access for TUI image paste.
//!
//! `arboard` does the platform work (ARCHITECTURE §4.8). The contract is that
//! **no failure ever escapes**. An
//! unusable clipboard reads as [`ClipboardContentType::Empty`] / `None`, and
//! every OS call is bounded by 3 s.
//!
//! Writing is the other direction and has its own shape:
//! [`NativeCopyWorker`] below. Reads are one-shot and can be abandoned, but a
//! write has to *stay* written — on X11/Wayland the writing process owns the
//! selection — so the write side keeps one long-lived thread and one
//! long-lived handle instead of opening a handle per call.

use std::cell::Cell;
use std::sync::mpsc;
use std::time::Duration;

use arboard::Clipboard;

const EXEC_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_NATIVE_COPY_JOBS: usize = 2;

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

/// Whether a clipboard can be opened at all.
pub fn is_clipboard_available() -> bool {
    Clipboard::new().is_ok()
}

// ── Write side: the bounded native copy worker (design D5) ──────────────────

/// Setting the host clipboard's text, as narrowly as the copy transport needs
/// it. The one seam tests replace: a fake records or blocks instead of
/// touching the machine running the suite.
pub trait TextClipboard: Send {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

/// Opens a [`TextClipboard`] **on the worker thread**.
///
/// `FnOnce` because the handle is opened once and kept: calling it a second
/// time would mean a second worker, which [`NativeCopyWorker`] never does.
pub type TextClipboardOpener = Box<dyn FnOnce() -> Result<Box<dyn TextClipboard>, String> + Send>;

/// `arboard`-backed writes, holding the handle open.
///
/// Unlike [`SystemClipboard`] this is *not* opened per call. On X11 and
/// Wayland the text lives in the writing process and is served to the pasting
/// app on request, so dropping the handle drops the selection — the classic
/// "copy worked, paste is empty" bug.
pub struct SystemTextClipboard(Clipboard);

impl SystemTextClipboard {
    /// Open a handle on the calling thread, which must also be the thread
    /// that writes through it.
    pub fn open() -> Result<Self, String> {
        Clipboard::new()
            .map(SystemTextClipboard)
            .map_err(|err| err.to_string())
    }
}

impl TextClipboard for SystemTextClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        self.0.set_text(text).map_err(|err| err.to_string())
    }
}

/// The production opener.
pub fn system_text_clipboard_opener() -> TextClipboardOpener {
    Box::new(|| {
        SystemTextClipboard::open().map(|handle| Box::new(handle) as Box<dyn TextClipboard>)
    })
}

/// How one submitted write ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCopyStatus {
    /// The OS accepted the text.
    Written,
    /// The clipboard could not be opened at all. Definite and permanent for
    /// this worker: every later job fails the same way.
    Unavailable(String),
    /// The handle was open but this write failed.
    Failed(String),
}

/// A finished job, correlated to its submission by `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCopyDone {
    pub id: u64,
    pub status: NativeCopyStatus,
}

/// Why a submission was refused. Neither case ever blocks the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCopySubmitError {
    /// Two jobs are outstanding, including undrained completions. Retryable.
    Busy,
    /// The worker thread is gone — it panicked, or could not be started.
    /// Not retryable: this worker will never run anything again.
    Gone,
}

/// One OS thread that writes the host clipboard, with at most two outstanding jobs.
///
/// The OS call cannot be cancelled or timed out from outside, which drives
/// every choice here:
///
/// * **One thread, ever.** A stuck `set_text` must not be answered by piling
///   up threads, so a full queue is refused ([`NativeCopySubmitError::Busy`])
///   rather than growing.
/// * **One handle, ever.** Opened on the thread by the opener and kept for the
///   worker's lifetime (see [`SystemTextClipboard`]).
/// * **No join on drop.** Dropping this closes the job channel, so the thread
///   exits after its current job — but a thread stuck in the OS would hang
///   shutdown forever if we waited for it, so we never do. The process exits
///   with it.
pub struct NativeCopyWorker {
    jobs: mpsc::SyncSender<Job>,
    done: mpsc::Receiver<NativeCopyDone>,
    /// Only the caller changes this count; the worker finishing a write does
    /// not free capacity until its completion is consumed.
    outstanding: Cell<usize>,
}

struct Job {
    id: u64,
    text: String,
}

impl NativeCopyWorker {
    /// Start the thread. The opener runs on it, not here, so this never
    /// touches the OS clipboard on the caller's thread.
    pub fn spawn(opener: TextClipboardOpener) -> Self {
        // Both admitted jobs must fit before the opener returns or the worker
        // is scheduled. The outstanding count bounds queued, running and
        // completed jobs together, independently of channel occupancy.
        let (jobs, job_rx) = mpsc::sync_channel::<Job>(MAX_NATIVE_COPY_JOBS);
        let (done_tx, done) = mpsc::channel();
        let started = std::thread::Builder::new()
            .name("orch-clipboard-write".into())
            .spawn(move || run_worker(opener, job_rx, done_tx));
        if let Err(error) = started {
            // `job_rx` went with the closure, so `jobs` is already
            // disconnected and every submission reports `Gone`.
            tracing::error!(%error, "could not start the clipboard writer thread");
        }
        NativeCopyWorker {
            jobs,
            done,
            outstanding: Cell::new(0),
        }
    }

    /// Hand over one write without ever blocking.
    pub fn submit(&self, id: u64, text: String) -> Result<(), NativeCopySubmitError> {
        let outstanding = self.outstanding.get();
        if outstanding >= MAX_NATIVE_COPY_JOBS {
            return Err(NativeCopySubmitError::Busy);
        }
        self.jobs
            .try_send(Job { id, text })
            .map_err(|err| match err {
                mpsc::TrySendError::Full(_) => NativeCopySubmitError::Busy,
                mpsc::TrySendError::Disconnected(_) => NativeCopySubmitError::Gone,
            })?;
        self.outstanding.set(outstanding + 1);
        Ok(())
    }

    /// Take every completion that has arrived. Never blocks, so a UI loop can
    /// call it once a frame.
    pub fn drain_done(&self) -> Vec<NativeCopyDone> {
        let mut done = Vec::new();
        while let Ok(completion) = self.done.try_recv() {
            done.push(completion);
        }
        self.outstanding.set(self.outstanding.get() - done.len());
        done
    }

    /// Wait up to `timeout` for one completion.
    ///
    /// For callers that have decided to block — tests, mainly. The UI loop
    /// uses [`NativeCopyWorker::drain_done`]; a frame must not wait on an OS
    /// clipboard call.
    pub fn wait_done(&self, timeout: Duration) -> Option<NativeCopyDone> {
        let done = self.done.recv_timeout(timeout).ok()?;
        self.outstanding.set(self.outstanding.get() - 1);
        Some(done)
    }
}

fn run_worker(
    opener: TextClipboardOpener,
    jobs: mpsc::Receiver<Job>,
    done: mpsc::Sender<NativeCopyDone>,
) {
    // `recv` ends with `Err` once the owner drops its sender, which is how
    // this thread learns to exit.
    match opener() {
        Ok(mut clipboard) => {
            while let Ok(job) = jobs.recv() {
                let status = match clipboard.set_text(&job.text) {
                    Ok(()) => NativeCopyStatus::Written,
                    Err(reason) => NativeCopyStatus::Failed(reason),
                };
                if done.send(NativeCopyDone { id: job.id, status }).is_err() {
                    return;
                }
            }
        }
        Err(reason) => {
            tracing::warn!(%reason, "the host clipboard could not be opened for writing");
            // The failure is permanent for this worker, so jobs are answered
            // immediately rather than retried per job: a retry loop against a
            // headless host costs seconds per copy and still fails.
            while let Ok(job) = jobs.recv() {
                let status = NativeCopyStatus::Unavailable(reason.clone());
                if done.send(NativeCopyDone { id: job.id, status }).is_err() {
                    return;
                }
            }
        }
    }
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

/// Run a blocking clipboard call with a 3 s bound. A hung call keeps its
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Every wait here is on a worker thread reaching a point the test has
    /// already made inevitable, so the bound only has to be longer than a
    /// thread spawn. It is never a stand-in for a sleep.
    const WAIT: Duration = Duration::from_secs(5);

    /// Records what was written, so a test can assert bytes without a real
    /// clipboard.
    #[derive(Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl TextClipboard for Recorder {
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(text.to_owned());
            Ok(())
        }
    }

    /// Stops inside `set_text` until the test lets go, which is how "in
    /// flight" is made a fact rather than a race.
    struct Blocker {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl TextClipboard for Blocker {
        fn set_text(&mut self, _text: &str) -> Result<(), String> {
            let _ = self.entered.send(());
            // `Err` means the test has finished and dropped its end; the
            // thread must then be free to exit rather than wait forever.
            let _ = self.release.recv();
            Ok(())
        }
    }

    fn blocking_worker() -> (NativeCopyWorker, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, entries) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let worker = NativeCopyWorker::spawn(Box::new(move || {
            Ok(Box::new(Blocker {
                entered,
                release: releases,
            }) as Box<dyn TextClipboard>)
        }));
        (worker, entries, release)
    }

    #[test]
    fn a_submitted_write_reaches_the_clipboard() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&written);
        let worker =
            NativeCopyWorker::spawn(Box::new(move || Ok(Box::new(Recorder(recorded)) as _)));

        worker.submit(7, "hello".into()).unwrap();

        assert_eq!(
            worker.wait_done(WAIT),
            Some(NativeCopyDone {
                id: 7,
                status: NativeCopyStatus::Written,
            })
        );
        assert_eq!(*written.lock().unwrap(), vec!["hello".to_owned()]);
    }

    #[test]
    fn an_unopenable_clipboard_fails_every_job_as_unavailable() {
        let worker = NativeCopyWorker::spawn(Box::new(|| Err("no display".into())));

        for id in [1, 2] {
            worker.submit(id, "text".into()).unwrap();
            assert_eq!(
                worker.wait_done(WAIT),
                Some(NativeCopyDone {
                    id,
                    status: NativeCopyStatus::Unavailable("no display".into()),
                }),
                "a failed open is definite, so later jobs fail the same way"
            );
        }
    }

    #[test]
    fn a_write_error_is_failed_not_unavailable() {
        struct Rejecting;
        impl TextClipboard for Rejecting {
            fn set_text(&mut self, _text: &str) -> Result<(), String> {
                Err("selection refused".into())
            }
        }
        let worker = NativeCopyWorker::spawn(Box::new(|| Ok(Box::new(Rejecting) as _)));

        worker.submit(1, "text".into()).unwrap();

        assert_eq!(
            worker.wait_done(WAIT),
            Some(NativeCopyDone {
                id: 1,
                status: NativeCopyStatus::Failed("selection refused".into()),
            })
        );
    }

    #[test]
    fn a_third_job_is_busy_rather_than_a_second_thread() {
        let (worker, entries, release) = blocking_worker();

        worker.submit(1, "first".into()).unwrap();
        // The first job is now *inside* `set_text`, so the queue slot is free.
        entries.recv_timeout(WAIT).unwrap();
        worker.submit(2, "second".into()).unwrap();

        assert_eq!(
            worker.submit(3, "third".into()),
            Err(NativeCopySubmitError::Busy),
            "one in flight plus one queued is the bound"
        );

        drop(release);
    }

    #[test]
    fn two_jobs_are_accepted_before_the_opener_returns() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&written);
        let (started, opening) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = NativeCopyWorker::spawn(Box::new(move || {
            started.send(()).unwrap();
            let _ = gate.recv();
            Ok(Box::new(Recorder(recorded)) as _)
        }));

        worker.submit(1, "first".into()).unwrap();
        opening.recv_timeout(WAIT).unwrap();
        worker.submit(2, "second".into()).unwrap();
        assert_eq!(worker.wait_done(Duration::ZERO), None);
        assert_eq!(
            worker.submit(3, "third".into()),
            Err(NativeCopySubmitError::Busy)
        );

        release.send(()).unwrap();
        assert_eq!(worker.wait_done(WAIT).map(|done| done.id), Some(1));
        assert_eq!(worker.wait_done(WAIT).map(|done| done.id), Some(2));
        worker.submit(3, "third".into()).unwrap();
        assert_eq!(worker.wait_done(WAIT).map(|done| done.id), Some(3));
        assert_eq!(*written.lock().unwrap(), vec!["first", "second", "third"]);
    }

    #[test]
    fn completed_jobs_hold_capacity_until_their_results_are_drained() {
        let (worker, entries, release) = blocking_worker();
        worker.submit(1, "first".into()).unwrap();
        entries.recv_timeout(WAIT).unwrap();
        worker.submit(2, "second".into()).unwrap();
        release.send(()).unwrap();
        // Entering the second write proves the first completion was sent.
        entries.recv_timeout(WAIT).unwrap();
        assert_eq!(
            worker.submit(3, "third".into()),
            Err(NativeCopySubmitError::Busy)
        );
        assert_eq!(
            worker.drain_done(),
            vec![NativeCopyDone {
                id: 1,
                status: NativeCopyStatus::Written,
            }]
        );
        worker.submit(3, "third".into()).unwrap();
        assert_eq!(
            worker.submit(4, "fourth".into()),
            Err(NativeCopySubmitError::Busy)
        );

        release.send(()).unwrap();
        entries.recv_timeout(WAIT).unwrap();
        assert_eq!(
            worker.drain_done(),
            vec![NativeCopyDone {
                id: 2,
                status: NativeCopyStatus::Written,
            }]
        );
        release.send(()).unwrap();
        assert_eq!(worker.wait_done(WAIT).map(|done| done.id), Some(3));
        worker.submit(4, "fourth".into()).unwrap();
        release.send(()).unwrap();
        assert_eq!(worker.wait_done(WAIT).map(|done| done.id), Some(4));
    }

    #[test]
    fn the_opener_runs_once_for_many_jobs() {
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        let worker = NativeCopyWorker::spawn(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Recorder::default()) as _)
        }));

        for id in 1..=5 {
            worker.submit(id, format!("body {id}")).unwrap();
            assert_eq!(worker.wait_done(WAIT).map(|done| done.id), Some(id));
        }

        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "the handle is opened once and kept, not reopened per job"
        );
    }

    #[test]
    fn dropping_the_worker_does_not_wait_for_a_stuck_write() {
        let (worker, entries, release) = blocking_worker();
        worker.submit(1, "stuck".into()).unwrap();
        entries.recv_timeout(WAIT).unwrap();

        // The job is wedged in the OS call and stays wedged: joining the
        // thread here would hang shutdown, so the drop must not.
        let started = std::time::Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < WAIT,
            "dropping the owner waited for the in-flight write"
        );

        drop(release);
    }

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
