use std::collections::VecDeque;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

/// Pre-allocated pool of reusable byte buffers for PTY output.
///
/// Buffers are acquired by the worker thread for overlapped reads and
/// automatically returned to the pool when the consumer drops the
/// `OutputBuffer` wrapper. In steady state, no allocations occur.
pub struct BufferPool {
    inner: Mutex<VecDeque<Vec<u8>>>,
    buf_capacity: usize,
}

impl BufferPool {
    pub fn new(buf_capacity: usize, initial_count: usize) -> Arc<Self> {
        let mut bufs = VecDeque::with_capacity(initial_count);
        for _ in 0..initial_count {
            bufs.push_back(vec![0u8; buf_capacity]);
        }
        Arc::new(Self {
            inner: Mutex::new(bufs),
            buf_capacity,
        })
    }

    /// Get a buffer from the pool, or allocate a new one if empty.
    pub fn acquire(self: &Arc<Self>) -> Vec<u8> {
        self.inner
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![0u8; self.buf_capacity])
    }

    /// Wrap a buffer (truncated to `len` bytes of valid data) into an
    /// `OutputBuffer` that auto-returns to this pool on drop.
    pub fn wrap(self: &Arc<Self>, mut buf: Vec<u8>, len: usize) -> OutputBuffer {
        buf.truncate(len);
        OutputBuffer {
            buf,
            original_capacity: self.buf_capacity,
            pool: Arc::clone(self),
        }
    }

    fn release(&self, mut buf: Vec<u8>) {
        buf.clear();
        if buf.capacity() >= self.buf_capacity {
            buf.resize(self.buf_capacity, 0);
            self.inner.lock().unwrap().push_back(buf);
        }
        // If capacity shrank (shouldn't happen), just drop it.
    }
}

/// Owned output data from a PTY read. Dereferences to `&[u8]`.
/// When dropped, the backing buffer is returned to the pool for reuse.
pub struct OutputBuffer {
    buf: Vec<u8>,
    #[allow(dead_code)] // reserved for capacity-validation in debug builds
    original_capacity: usize,
    pool: Arc<BufferPool>,
}

impl Deref for OutputBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.buf
    }
}

impl AsRef<[u8]> for OutputBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.buf
    }
}

impl std::fmt::Debug for OutputBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputBuffer")
            .field("len", &self.buf.len())
            .finish()
    }
}

impl Drop for OutputBuffer {
    fn drop(&mut self) {
        let buf = std::mem::take(&mut self.buf);
        self.pool.release(buf);
    }
}
