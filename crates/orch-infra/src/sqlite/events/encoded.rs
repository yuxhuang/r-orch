use super::*;
use std::io::Write;

#[derive(Debug)]
pub(in crate::sqlite) struct EncodedEvent {
    pub seq: u64,
    pub at: i64,
    pub kind: &'static str,
    pub payload: Box<str>,
}

impl EncodedEvent {
    pub(in crate::sqlite) fn rows(&self) -> u8 {
        if self.payload.len() > crate::blob::INLINE_TEXT_MAX_BYTES {
            2
        } else {
            1
        }
    }

    pub(in crate::sqlite) fn encode(row: &SequencedEvent, limit: usize) -> Option<Self> {
        struct Bounded {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let size = self
                    .bytes
                    .len()
                    .checked_add(bytes.len())
                    .filter(|size| *size <= self.limit)
                    .ok_or_else(|| std::io::Error::other("event exceeds available bytes"))?;
                if size > self.bytes.capacity() {
                    let growth = (size - self.bytes.len())
                        .max(64 * 1024)
                        .min(self.limit - self.bytes.len());
                    self.bytes
                        .try_reserve_exact(growth)
                        .map_err(std::io::Error::other)?;
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut out = Bounded {
            bytes: Vec::new(),
            limit,
        };
        serde_json::to_writer(&mut out, &row.event).ok()?;
        // serde_json writes UTF-8. Keep the exact encoding, without a fresh JSON parse.
        let payload = String::from_utf8(out.bytes).ok()?.into_boxed_str();
        Some(Self {
            seq: row.seq,
            at: row
                .event
                .timestamp
                .parse::<jiff::Timestamp>()
                .map_or(0, |t| t.as_millisecond()),
            kind: row.event.event_type.as_str(),
            payload,
        })
    }
}

pub(in crate::sqlite) fn matches(
    conn: &Connection,
    project: &str,
    run: &str,
    event: &EncodedEvent,
    disk: &DiskBlobStore,
) -> Result<Option<bool>> {
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT at,type,payload FROM run_events WHERE project_id=?1 AND run_id=?2 AND seq=?3",
            params![project, run, event.seq as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((at, kind, payload)) = row else {
        return Ok(None);
    };
    // An event's frozen encoding is an object. Only the separate stored tag can
    // select a blob; data inside the object cannot do so. Avoid a decoded tree.
    let payload = if payload.starts_with('{') {
        payload
    } else {
        let tag: [String; 2] = serde_json::from_str(&payload)
            .map_err(|_| EventsError::Corrupt("invalid event blob reference".into()))?;
        if tag[0] != BLOB_TAG {
            return Err(EventsError::Corrupt("invalid event blob reference".into()));
        }
        load_payload(conn, project, &payload, Some(disk))?
    };
    Ok(Some(
        at == event.at && kind == event.kind && payload == event.payload.as_ref(),
    ))
}
