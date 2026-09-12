use std::{collections::VecDeque, sync::Mutex};

pub(crate) struct RingBuffer {
    inner: Mutex<RingBufferInner>,
    max_bytes: usize,
}

struct RingBufferInner {
    chunks: VecDeque<OutputChunk>,
    next_seq: u64,
    current_bytes: usize,
}

struct OutputChunk {
    seq: u64,
    bytes: Vec<u8>,
    truncated_prefix: bool,
}

impl RingBuffer {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(RingBufferInner {
                chunks: VecDeque::new(),
                next_seq: 1,
                current_bytes: 0,
            }),
            max_bytes,
        }
    }

    pub(crate) fn push(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.next_seq;
        inner.next_seq += 1;
        let truncated_prefix = bytes.len() > self.max_bytes;
        let retained = if truncated_prefix {
            &bytes[bytes.len() - self.max_bytes..]
        } else {
            bytes
        };
        inner.current_bytes += retained.len();
        inner.chunks.push_back(OutputChunk {
            seq,
            bytes: retained.to_vec(),
            truncated_prefix,
        });

        while inner.current_bytes > self.max_bytes {
            if let Some(old) = inner.chunks.pop_front() {
                inner.current_bytes = inner.current_bytes.saturating_sub(old.bytes.len());
            } else {
                break;
            }
        }
    }

    pub(crate) fn read_since(&self, since_seq: u64, max_bytes: usize) -> (Vec<u8>, u64, bool) {
        let inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let mut truncated = inner.chunks.front().is_some_and(|first| {
            since_seq.saturating_add(1) < first.seq
                || (first.truncated_prefix && first.seq > since_seq)
        });

        for chunk in inner.chunks.iter().filter(|chunk| chunk.seq > since_seq) {
            if out.len() + chunk.bytes.len() > max_bytes {
                let remaining = max_bytes.saturating_sub(out.len());
                out.extend_from_slice(&chunk.bytes[..remaining.min(chunk.bytes.len())]);
                truncated = true;
                break;
            }
            out.extend_from_slice(&chunk.bytes);
        }

        (out, inner.next_seq.saturating_sub(1), truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_incrementally() {
        let buffer = RingBuffer::new(1024);
        buffer.push(b"one");
        let (first, seq, truncated) = buffer.read_since(0, 1024);
        assert_eq!(first, b"one");
        assert_eq!(seq, 1);
        assert!(!truncated);

        buffer.push(b"two");
        let (second, seq, truncated) = buffer.read_since(seq, 1024);
        assert_eq!(second, b"two");
        assert_eq!(seq, 2);
        assert!(!truncated);
    }

    #[test]
    fn drops_old_chunks_when_capacity_is_exceeded() {
        let buffer = RingBuffer::new(4);
        buffer.push(b"abc");
        buffer.push(b"def");
        let (output, seq, truncated) = buffer.read_since(0, 1024);
        assert_eq!(output, b"def");
        assert_eq!(seq, 2);
        assert!(truncated);
    }

    #[test]
    fn keeps_tail_of_oversized_single_chunk() {
        let buffer = RingBuffer::new(4);
        buffer.push(b"abcdef");
        let (output, seq, truncated) = buffer.read_since(0, 1024);
        assert_eq!(output, b"cdef");
        assert_eq!(seq, 1);
        assert!(truncated);
    }
}
