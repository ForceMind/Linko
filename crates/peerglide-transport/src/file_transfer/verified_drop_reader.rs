use super::{PreparedDropPath, CHUNK_BYTES};
use anyhow::{ensure, Result};

/// Seekable content on the original received-file handle, verified by chunk.
pub struct VerifiedDropReader {
    prepared: PreparedDropPath,
    cached: Option<(usize, Vec<u8>)>,
}

impl PreparedDropPath {
    pub fn into_verified_reader(self) -> VerifiedDropReader {
        VerifiedDropReader {
            prepared: self,
            cached: None,
        }
    }
}

impl VerifiedDropReader {
    pub fn size(&self) -> u64 {
        self.prepared.manifest.size()
    }

    /// The native adapter only requests bounded pieces of the admitted stream.
    pub fn read_at(&mut self, offset: u64, mut output: &mut [u8]) -> Result<()> {
        let end = offset.checked_add(output.len() as u64);
        ensure!(
            output.len() <= CHUNK_BYTES && end.is_some_and(|end| end <= self.size()),
            "native stream read outside bounds"
        );
        ensure!(
            self.prepared._source.metadata()?.len() == self.size(),
            "prepared drop source size changed"
        );
        let mut offset = offset;
        while !output.is_empty() {
            let index = (offset / CHUNK_BYTES as u64) as usize;
            let start = (offset % CHUNK_BYTES as u64) as usize;
            if self.cached.as_ref().is_none_or(|(chunk, _)| *chunk != index) {
                let bytes = self
                    .prepared
                    .manifest
                    .read_chunk(&mut self.prepared._source, index)?;
                self.cached = Some((index, bytes));
            }
            let (_, chunk) = self.cached.as_ref().expect("verified chunk is cached");
            let count = output.len().min(chunk.len() - start);
            let (head, tail) = output.split_at_mut(count);
            head.copy_from_slice(&chunk[start..start + count]);
            output = tail;
            offset += count as u64;
        }
        Ok(())
    }
}
