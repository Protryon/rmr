use std::{
    io::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

pub struct ObservableBuf<'a> {
    pub inner: &'a mut Vec<u8>,
    pub len: Arc<AtomicUsize>,
}

impl<'a> ObservableBuf<'a> {
    pub fn new(inner: &'a mut Vec<u8>) -> (Self, Arc<AtomicUsize>) {
        let len = Arc::new(AtomicUsize::new(inner.len()));
        (
            Self {
                inner,
                len: len.clone(),
            },
            len,
        )
    }
}

impl<'a> Write for ObservableBuf<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let out = self.inner.write(buf);
        self.len.store(self.inner.len(), Ordering::SeqCst);
        out
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
