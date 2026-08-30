//! qpdf correspondence: Pl_Count.cc byte-count, last-byte, forwarding, and finish responsibilities.

use super::{Pipeline, PipelineError, PipelineResult};

pub(crate) struct Count<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    count: u64,
    last_byte: u8,
}

impl<'a> Count<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            count: 0,
            last_byte: 0,
        }
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn last_byte(&self) -> u8 {
        self.last_byte
    }
}

impl Pipeline for Count<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if data.is_empty() {
            return Ok(());
        }

        self.count = self.count.checked_add(data.len() as u64).ok_or_else(|| {
            PipelineError::runtime(format!("{}: byte count overflow", self.identifier))
        })?;
        self.last_byte = data[data.len() - 1];
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.next.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Count;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    #[derive(Default)]
    struct RecordingSink {
        chunks: Vec<Vec<u8>>,
        finishes: usize,
    }

    impl Pipeline for RecordingSink {
        fn identifier(&self) -> &str {
            "recording"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn count_ignores_empty_writes_and_is_reusable_after_finish() {
        let mut sink = RecordingSink::default();
        assert_eq!(sink.identifier(), "recording");
        {
            let mut count = Count::new("count", &mut sink);
            assert_eq!(count.identifier(), "count");
            count.write(b"abc").unwrap();
            count.write(b"").unwrap();
            assert_eq!(count.count(), 3);
            assert_eq!(count.last_byte(), b'c');
            count.finish().unwrap();
            count.write(b"d").unwrap();
            assert_eq!(count.count(), 4);
            assert_eq!(count.last_byte(), b'd');
        }
        assert_eq!(sink.chunks, vec![b"abc".to_vec(), b"d".to_vec()]);
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn empty_count_reports_qpdf_defaults() {
        let mut sink = RecordingSink::default();
        let count = Count::new("count", &mut sink);
        assert_eq!(count.count(), 0);
        assert_eq!(count.last_byte(), 0);
    }

    #[test]
    fn count_rejects_overflow_without_forwarding_the_chunk() {
        let mut sink = RecordingSink::default();
        let mut count = Count::new("count", &mut sink);
        count.count = u64::MAX;

        assert!(matches!(
            count.write(b"x").unwrap_err(),
            PipelineError::Runtime(_)
        ));
        assert_eq!(count.count(), u64::MAX);
        assert_eq!(count.last_byte(), 0);
        drop(count);
        assert!(sink.chunks.is_empty());
    }
}
