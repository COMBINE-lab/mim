use crate::gzip_reader::GzipStreamReader;
use crate::types::MimIndex;
use anyhow::Result;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Type managing multithreaded parsing of a .gz file with a .mim index.
// TODO: Parser? Reader?
pub struct MimReader {
    /// .gz input file.
    pub input_path: PathBuf,
    /// The index itself.
    pub index: MimIndex,
    /// The number of worker threads.
    pub num_workers: usize,
    /// The range of chunks assigned to each worker.
    pub chunk_assignments: Vec<Range<usize>>,
}

impl MimReader {
    /// Create a `MimReader` for the given `.gz` and associated `.gz.mim` file, and number of worker threads.
    pub fn new(gz_path: &Path, num_workers: usize) -> Self {
        Self::new_with_index(gz_path, &gz_path.with_added_extension("mim"), num_workers)
    }

    /// Create a `MimReader` for the given `.gz` and optional `.mim` file, and number of worker threads.
    pub fn new_with_opt_index(
        gz_path: &Path,
        index_path: Option<&Path>,
        num_workers: usize,
    ) -> Self {
        let index_path = match index_path {
            Some(p) => p.to_owned(),
            None => gz_path.with_added_extension("mim"),
        };
        Self::new_with_index(gz_path, &index_path, num_workers)
    }

    /// Create a `MimReader` for the given `.gz` and `.mim` files, and number of worker threads.
    pub fn new_with_index(gz_path: &Path, index_path: &Path, num_workers: usize) -> Self {
        let index = MimIndex::read(&index_path).expect("failed to load index");
        Self {
            num_workers,
            input_path: gz_path.to_owned(),
            chunk_assignments: index.distribute_chunks(num_workers),
            index,
        }
    }

    /// A reader of the record-aligned byte range of this worker.
    pub fn readers(&self) -> impl Iterator<Item = Result<GzipStreamReader>> {
        (0..self.num_workers).map(|worker_id| self.get_reader(worker_id))
    }

    /// A reader of the record-aligned byte range of this worker.
    pub fn get_reader(&self, worker_id: usize) -> Result<GzipStreamReader> {
        if worker_id >= self.num_workers {
            anyhow::bail!(
                "Requested work for worker {}, but only {} workers were registered.",
                worker_id,
                self.num_workers
            )
        }

        Ok(GzipStreamReader::read_range(
            &self.input_path,
            &self.index,
            (&self.chunk_assignments)[worker_id].clone(),
        )?)
    }

    /// A [`needletail::FastxReader`] over the records of this worker.
    ///
    /// Convenience wrapper around [`Self::get_reader`].
    #[cfg(feature = "needletail")]
    pub fn get_needletail_parser<'a>(
        &'a self,
        worker_id: usize,
    ) -> Result<Box<dyn needletail::FastxReader + 'a>, needletail::errors::ParseError> {
        needletail::parse_fastx_reader(
            self.get_reader(worker_id)
                .expect("could not get byte-limited stream"),
        )
    }

    /// A lending iterator over the [`needletail::parser::SequenceRecord`] records records of this worker.
    ///
    /// Convenience wrapper around [`Self::get_reader`] and [`Self::get_needletail_parser`].
    #[cfg(feature = "needletail")]
    pub fn get_needletail_iter<'a>(&'a self, worker_id: usize) -> Result<ReadIter<'a>> {
        let stream = self.get_reader(worker_id)?;
        let fastx_reader = needletail::parse_fastx_reader(stream).expect("invalid reader");

        Ok(ReadIter::new(fastx_reader))
    }
}

/// Synchronous multithreaded parsing of multiple files.
#[cfg(feature = "needletail")]
pub struct MultiMimReader {
    /// .gz input files.
    pub input_paths: Vec<PathBuf>,
    /// The index itself.
    pub indexes: Vec<MimIndex>,
    /// The number of worker threads.
    pub num_workers: usize,
    /// The range of chunks assigned to each worker.
    /// Split is on the first input file.
    pub chunk_assignments: Vec<Range<usize>>,
}

#[cfg(feature = "needletail")]
impl MultiMimReader {
    pub fn new_with_workers<P: AsRef<Path>>(
        gz_paths: &[P],
        index_paths: &[P],
        num_workers: usize,
    ) -> anyhow::Result<Self> {
        assert_eq!(gz_paths.len(), index_paths.len());
        assert!(!gz_paths.is_empty());

        let indexes: Vec<MimIndex> = index_paths
            .iter()
            .map(|path| MimIndex::read(path.as_ref()).expect("failed to load index"))
            .collect();

        // distribute chunks based on the first file
        let chunk_assignments = indexes
            .first()
            .expect("at least two indexes")
            .distribute_chunks(num_workers);

        Ok(Self {
            num_workers,
            input_paths: gz_paths.iter().map(|f| f.as_ref().to_owned()).collect(),
            chunk_assignments,
            indexes,
        })
    }

    pub fn get_needletail_parsers<'a>(
        &'a self,
    ) -> impl Iterator<Item = anyhow::Result<Vec<Box<dyn needletail::FastxReader + 'a>>>> {
        (0..self.num_workers).map(|worker_id| {
            let reader0 = GzipStreamReader::read_range(
                &self.input_paths[0],
                &self.indexes[0],
                (&self.chunk_assignments)[worker_id].clone(),
            )?;
            let first_record_rank = reader0.record_idx_range().start;

            let mut parsers = Vec::with_capacity(self.input_paths.len());
            let parser0 = needletail::parse_fastx_reader(reader0).expect("invalid reader");
            parsers.push(parser0);

            for i in 1..self.input_paths.len() {
                // Synchronize files.
                // Fine the last chunk with start <= first_record_rank.
                let checkpoint = self.indexes[i]
                    .record_boundaries
                    .partition_point(|x| x.next_record_idx <= first_record_rank)
                    - 1;
                // and the record rank starting this chunk
                let checkpoint_rank = self.indexes[i].record_boundaries[checkpoint].next_record_idx;
                let skip = first_record_rank - checkpoint_rank;

                let reader_i = GzipStreamReader::read_from_checkpoint(
                    &self.input_paths[i],
                    &self.indexes[i],
                    checkpoint,
                )?;
                let mut parser_i = needletail::parse_fastx_reader(reader_i)?;
                for _ in 0..skip {
                    let _ = parser_i.next();
                }
                parsers.push(parser_i);
            }

            Ok(parsers)
        })
    }
}

#[cfg(feature = "needletail")]
mod record_iter {
    use lender::prelude::*;
    use needletail::errors::ParseError;
    use needletail::parser::{FastxReader, SequenceRecord};

    /// An iterator over [`needletail::parser::SequenceRecord`] records.
    pub struct ReadIter<'a> {
        reader: Box<dyn FastxReader + 'a>,
    }

    impl<'a> ReadIter<'a> {
        pub fn new(reader: Box<dyn FastxReader + 'a>) -> Self {
            Self { reader }
        }
    }

    impl<'this, 'lend> Lending<'lend> for ReadIter<'this> {
        type Lend = Result<SequenceRecord<'lend>, ParseError>;
    }

    #[allow(clippy::should_implement_trait)]
    impl<'this> Lender for ReadIter<'this> {
        fn next(&mut self) -> Option<Lend<'_, Self>> {
            self.reader.next()
        }
    }
}

#[cfg(feature = "needletail")]
pub use record_iter::ReadIter;
