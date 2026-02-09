//! FIXME: Split work by chunk-size, not just by chunk-count.
use crate::gzip_reader::GzipStreamReader;
use crate::mim_types::MimIndex;
use anyhow::Result;
use lender::prelude::*;
use needletail::errors::ParseError;
use needletail::parser::{FastxReader, SequenceRecord};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

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

pub struct MultiParser {
    pub nworker: usize,
    pub fpath: PathBuf,
    pub ipath: PathBuf,
    pub chunk_assignments: Vec<Range<usize>>,
    pub index: MimIndex,
}

pub struct MultiPairParser {
    pub nworker: usize,
    pub fpaths: Vec<PathBuf>,
    pub ipaths: Vec<PathBuf>,
    /// chunk assignments are with respect to read1 files
    pub chunk_assignments: Vec<Range<usize>>,
    pub indexes: Vec<MimIndex>,
}

impl MultiParser {
    pub fn new_with_workers(fpath: &Path, ipath: &Path, nworker: usize) -> Self {
        let index = MimIndex::read(ipath).expect("failed to load index");

        let chunk_assignments = index.distribute_chunks(nworker);

        Self {
            nworker,
            fpath: PathBuf::from(fpath),
            ipath: PathBuf::from(ipath),
            chunk_assignments,
            index,
        }
    }

    /// Returns a reader that returns exactly the chunk for the given worker.
    pub fn get_worker_stream(&self, worker_id: usize) -> Result<GzipStreamReader> {
        if worker_id >= self.nworker {
            anyhow::bail!(
                "Requested work for worker {}, but only {} workers were registered.",
                worker_id,
                self.nworker
            )
        }

        let gzfq = GzipStreamReader::open_for_checkpoint_range(
            &self.fpath,
            &self.index,
            (&self.chunk_assignments)[worker_id].clone(),
        )?;
        Ok(gzfq)
    }

    pub fn get_needletail_parser_for_worker<'a>(
        &'a self,
        worker_id: usize,
    ) -> Result<Box<dyn FastxReader + 'a>, ParseError> {
        let byte_limited_stream = self
            .get_worker_stream(worker_id)
            .expect("could not get byte-limited stream");
        // trace!("Worker {worker_id} will yield {nbytes} total uncompressed bytes");
        needletail::parse_fastx_reader(byte_limited_stream)
    }

    pub fn get_worker_iter<'a>(&'a self, worker_id: usize) -> Result<ReadIter<'a>> {
        let stream = self.get_worker_stream(worker_id)?;
        let fastx_reader = needletail::parse_fastx_reader(stream).expect("invalid reader");

        Ok(ReadIter {
            reader: fastx_reader,
        })
    }
}

impl MultiPairParser {
    pub fn new_with_workers<P: AsRef<Path>>(
        fpaths: &[P],
        ipaths: &[P],
        nworker: usize,
    ) -> anyhow::Result<Self> {
        // for now, we only handle a single pair of reads
        assert_eq!(fpaths.len(), 2);
        assert_eq!(ipaths.len(), 2);

        let indexes: Vec<MimIndex> = ipaths
            .iter()
            .map(|path| MimIndex::read(path.as_ref()).expect("failed to load index"))
            .collect();

        // distribute chunks based on the first file
        let chunk_assignments = indexes
            .first()
            .expect("at least two indexes")
            .distribute_chunks(nworker);

        Ok(Self {
            nworker,
            fpaths: fpaths
                .iter()
                .map(|f| PathBuf::from(f.as_ref()))
                .collect::<Vec<PathBuf>>(),
            ipaths: ipaths
                .iter()
                .map(|f| PathBuf::from(f.as_ref()))
                .collect::<Vec<PathBuf>>(),
            chunk_assignments,
            indexes,
        })
    }

    pub fn get_needletail_parsers_for_worker<'a>(
        &'a self,
        worker_id: usize,
    ) -> anyhow::Result<(Box<dyn FastxReader + 'a>, Box<dyn FastxReader + 'a>)> {
        if worker_id < self.nworker {
            let gzfq = GzipStreamReader::open_for_checkpoint_range(
                &self.fpaths[0],
                &self.indexes[0],
                (&self.chunk_assignments)[worker_id].clone(),
            )?;
            let first_record_rank = gzfq.record_idx_range().start;
            // now sync the second file up with the first
            // find the chunk we jump to in file 2
            let mut second_file_chunk = self.indexes[1]
                .record_boundaries
                .partition_point(|x| x.next_record_idx < first_record_rank);
            if self.indexes[1].record_boundaries[second_file_chunk].next_record_idx
                > first_record_rank
            {
                second_file_chunk = 0_i64.max(second_file_chunk as i64 - 1) as usize;
            }
            // and the record rank starting this chunk
            let second_record_rank =
                self.indexes[1].record_boundaries[second_file_chunk].next_record_idx;

            let mut gzfq2 = GzipStreamReader::open_at_checkpoint(
                &self.fpaths[1],
                &self.indexes[1],
                second_file_chunk,
            )?;
            // skip the byte sup to the first record
            let record_offset =
                self.indexes[1].record_boundaries[second_file_chunk].next_record_pos;
            if record_offset > gzfq2.out_pos() {
                // discard the requisite number of bytes
                let mut discard_buf = vec![0_u8; (record_offset - gzfq2.out_pos()) as usize];
                gzfq2.read_exact(&mut discard_buf)?;
            }

            let r1 = needletail::parse_fastx_reader(gzfq)?;
            let mut r2 = needletail::parse_fastx_reader(gzfq2)?;

            // skip the reads to sync up with reader 1
            let reads_to_skip = first_record_rank - second_record_rank;
            for _ in 0..reads_to_skip {
                let _ = r2.next();
            }

            Ok((r1, r2))
        } else {
            anyhow::bail!(
                "Requested work for worker {}, but only {} workers were registered.",
                worker_id,
                self.nworker
            )
        }
    }
}
