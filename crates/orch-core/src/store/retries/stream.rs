//! Bounded, provisional retry diagnostic reads (T12 first-use decision).
//!
//! This additive port leaves the whole-value retry methods unchanged. A text
//! window is a byte slice, not a `str`: UTF-8 may span windows and objects.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::RetryCursor;
use crate::store::StoreError;

/// Maximum bytes supplied by one synchronous sink callback.
pub const RETRY_STREAM_WINDOW_BYTES: usize = 65_536;

/// Content-free streaming failures; admission is never persisted-data damage.
#[derive(Debug, thiserror::Error)]
pub enum RetryStreamError {
    #[error("retry text resources unavailable")]
    Resource,
    #[error("retry text read cancelled")]
    Cancelled,
    #[error("stored retry text is corrupt")]
    Corrupt,
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub type Result<T> = std::result::Result<T, RetryStreamError>;

/// Borrowed metadata of the selected row. No complete diagnostic is allocated.
#[derive(Debug, Clone, Copy)]
pub struct RetryTextRow<'a> {
    pub task: &'a str,
    pub attempt: u32,
    pub due_at: i64,
    /// Logical UTF-8 bytes, checked during preflight and again while reading.
    pub bytes: u64,
}

/// A bounded sink, called only on the storage adapter's blocking boundary.
///
/// `begin` and `write` are PROVISIONAL, including callbacks for earlier rows.
/// Return from `write` supplies backpressure. The sink must not retain borrowed
/// windows or accumulate an unbounded string. Any retained memory must hold an
/// admission permit from the adapter's shared budget, supplied to both callbacks. A file sink can instead
/// spool provisional bytes and discard them on failure.
///
/// Only the successful enclosing `read_retry_text` result authorizes publication
/// and cursor advancement. On any failure, cancellation or dropped future the
/// consumer discards the entire provisional page. An implementation must latch
/// callback errors, never return success after swallowing one, and stop calling
/// the sink after cancellation is observed.
pub trait RetryTextSink: Send + 'static {
    fn begin(&mut self, row: RetryTextRow<'_>, admission: &RetryStreamAdmission) -> Result<()>;
    fn write(&mut self, bytes: &[u8], admission: &RetryStreamAdmission) -> Result<()>;
}

/// Returned only after all selected rows and the whole page have verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryTextPage {
    pub rows: usize,
    /// Sum of logical row lengths, using checked arithmetic; no size ceiling.
    pub bytes: u64,
    pub next: Option<RetryCursor>,
}

/// Bounded whole-text recovery from the enclosing committed snapshot.
///
/// Select exactly the same rows/order/cursor as `RetryRead::read_retries`.
/// Preflight declared lengths and checked page accounting before opening any
/// object. Admit metadata, JSON and windows before materializing them; allocation
/// and admission failures return `Resource`, with no completed page. Validate
/// every ordered object occurrence, actual lengths and hashes, whole-value hash
/// and incremental UTF-8. Do not inspect damaged unselected rows.
///
/// Adapters retain one shared budget across snapshots and concurrent reads,
/// including queued work. A request's logical length is not an admission ceiling.
/// Snapshot ownership outlives blocking work. The adapter creates a child token
/// and holds its drop guard across the await: dropping this future cancels the
/// worker, whose admission remains charged until it actually exits. Check that
/// token before reads, before each callback and before returning success. A
/// caller-supplied cancellation also cancels the child; cancelling one read must
/// not cancel another. No production caller is authorized by this trait alone.
#[async_trait]
pub trait RetryTextRead: Send + Sync {
    async fn read_retry_text(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
        sink: Box<dyn RetryTextSink>,
        cancellation: CancellationToken,
    ) -> Result<RetryTextPage>;
}

struct Admission {
    limit: usize,
    used: AtomicUsize,
}

/// A shared byte counter for actual live streaming work, never logical length.
/// Clones share capacity. Adapters own it and keep it across snapshots; callers
/// cannot replace an adapter's budget to bypass concurrent admission.
#[derive(Clone)]
pub struct RetryStreamAdmission(Arc<Admission>);

impl RetryStreamAdmission {
    pub fn new(bytes: usize) -> Self {
        Self(Arc::new(Admission {
            limit: bytes,
            used: AtomicUsize::new(0),
        }))
    }

    /// Charge before allocating or queuing work. Overflow and exhaustion refuse
    /// immediately; no partial charge remains. Keep the permit until work exits,
    /// including after the requesting future has been cancelled.
    pub fn reserve(&self, bytes: usize) -> Result<RetryStreamPermit> {
        self.0
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.0.limit)
            })
            .map_err(|_| RetryStreamError::Resource)?;
        Ok(RetryStreamPermit {
            admission: self.clone(),
            bytes,
        })
    }

    /// A fallible, charged byte buffer. Capacity errors and allocation failures
    /// release their charge. This accounts payload capacity, not allocator
    /// bookkeeping or the adapter's pre-existing snapshot/database state.
    pub fn buffer(&self, bytes: usize) -> Result<RetryStreamBuffer> {
        let mut permit = self.reserve(bytes)?;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(bytes)
            .map_err(|_| RetryStreamError::Resource)?;
        // Vec permits an allocator to return more capacity than requested.
        // Charge that excess before the buffer can escape this function.
        if buffer.capacity() > bytes {
            let extra = buffer.capacity() - bytes;
            let mut excess = self.reserve(extra)?;
            permit.bytes += extra; // Sum equals the representable Vec capacity.
            excess.bytes = 0; // Transfer its existing charge to the main permit.
        }
        buffer.resize(bytes, 0);
        Ok(RetryStreamBuffer {
            buffer,
            _permit: permit,
        })
    }
}

/// Not Clone: precisely one owner releases each charge, once.
pub struct RetryStreamPermit {
    admission: RetryStreamAdmission,
    bytes: usize,
}

impl Drop for RetryStreamPermit {
    fn drop(&mut self) {
        self.admission
            .0
            .used
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// A fixed-length buffer whose shared admission remains held until drop.
/// Capacity cannot grow through this interface.
pub struct RetryStreamBuffer {
    // Field order drops the payload before releasing its admission.
    buffer: Vec<u8>,
    _permit: RetryStreamPermit,
}

impl RetryStreamBuffer {
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_admission_checks_overflow_and_releases_on_worker_exit() {
        let budget = RetryStreamAdmission::new(8);
        let permit = budget.reserve(5).unwrap();
        assert!(matches!(budget.reserve(4), Err(RetryStreamError::Resource)));
        assert!(matches!(
            budget.reserve(usize::MAX),
            Err(RetryStreamError::Resource)
        ));
        std::thread::scope(|scope| {
            let clone = budget.clone();
            scope
                .spawn(move || {
                    let _permit = permit;
                    assert!(clone.reserve(4).is_err());
                    let _rest = clone.reserve(3).unwrap();
                })
                .join()
                .unwrap();
        });
        assert!(budget.reserve(8).is_ok());
    }

    #[test]
    fn fallible_buffer_failure_returns_resource_and_releases_admission() {
        // Vec cannot represent this capacity, independent of allocator behavior.
        let budget = RetryStreamAdmission::new(usize::MAX);
        assert!(matches!(
            budget.buffer(usize::MAX),
            Err(RetryStreamError::Resource)
        ));
        let mut window = budget.buffer(RETRY_STREAM_WINDOW_BYTES).unwrap();
        assert_eq!(window.as_mut_slice().len(), RETRY_STREAM_WINDOW_BYTES);
        window.as_mut_slice().fill(b'x');
        drop(window);
        let _all = budget.reserve(usize::MAX).unwrap();
    }

    #[test]
    fn a_buffer_holds_its_charge_until_drop() {
        let budget = RetryStreamAdmission::new(8);
        let window = budget.buffer(8).unwrap();
        assert!(budget.reserve(1).is_err());
        drop(window);
        assert!(budget.buffer(8).is_ok());
    }

    #[test]
    fn request_drop_cancels_only_its_child_and_keeps_worker_admission() {
        let parent = CancellationToken::new();
        let sibling = parent.child_token();
        let child = parent.child_token();
        let budget = RetryStreamAdmission::new(8);
        let worker_permit = budget.reserve(8).unwrap();
        let guard = child.clone().drop_guard();
        drop(guard);
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
        assert!(!sibling.is_cancelled());
        assert!(budget.reserve(1).is_err());
        drop(worker_permit);
        assert!(budget.reserve(8).is_ok());
    }
}
