//! Bounded JSONL reader: rejected consecutive lines become one gap.
use super::document::{Limits, Refusal, check_json};
use serde::de::DeserializeOwned;
use std::io::{BufRead, BufReader, Read};

#[derive(Debug, PartialEq)]
pub(crate) enum EventItem<T> {
    Event { line: u64, value: T },
    Gap { first: u64, last: u64 },
}

/// Retains at most `line_bytes + 1` bytes of a line, even without a newline.
/// Dropping the stream stops reading; consumers control batching and disk checks.
pub(crate) struct EventStream<R, T> {
    reader: BufReader<R>,
    limits: Limits,
    line: u64,
    pending: Option<EventItem<T>>,
    finished: bool,
}
impl<R: Read, T: DeserializeOwned> EventStream<R, T> {
    pub(crate) fn new(reader: R, limits: Limits) -> Self {
        Self {
            reader: BufReader::with_capacity(8192, reader),
            limits,
            line: 0,
            pending: None,
            finished: false,
        }
    }
    fn line(&mut self) -> Result<Option<Vec<u8>>, Refusal> {
        let mut bytes = Vec::new();
        let ceiling = self.limits.bytes.saturating_add(1);
        let mut seen = false;
        loop {
            let chunk = match self.reader.fill_buf() {
                Ok(chunk) => chunk,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Refusal::invalid("io", error)),
            };
            if chunk.is_empty() {
                return Ok(seen.then_some(bytes));
            }
            seen = true;
            let newline = chunk.iter().position(|b| *b == b'\n');
            let count = newline.unwrap_or(chunk.len());
            let retain = count.min(ceiling.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&chunk[..retain]);
            self.reader.consume(count + usize::from(newline.is_some()));
            if newline.is_some() {
                return Ok(Some(bytes));
            }
        }
    }
}
impl<R: Read, T: DeserializeOwned> Iterator for EventStream<R, T> {
    type Item = Result<EventItem<T>, Refusal>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.pending.take() {
            return Some(Ok(item));
        }
        if self.finished {
            return None;
        }
        let mut gap = None;
        loop {
            let bytes = match self.line() {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    self.finished = true;
                    return gap.map(Ok);
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            };
            self.line = match self.line.checked_add(1) {
                Some(line) => line,
                None => {
                    self.finished = true;
                    return Some(Err(Refusal::invalid("lines", "line count overflow")));
                }
            };
            let value = check_json(&bytes, self.limits)
                .ok()
                .and_then(|()| serde_json::from_slice::<T>(&bytes).ok());
            if let Some(value) = value {
                let event = EventItem::Event {
                    line: self.line,
                    value,
                };
                if let Some(gap) = gap {
                    self.pending = Some(event);
                    return Some(Ok(gap));
                }
                return Some(Ok(event));
            }
            gap = Some(match gap {
                Some(EventItem::Gap { first, .. }) => EventItem::Gap {
                    first,
                    last: self.line,
                },
                _ => EventItem::Gap {
                    first: self.line,
                    last: self.line,
                },
            });
        }
    }
}
