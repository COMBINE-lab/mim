use crate::gzip_reader::GzipStreamReader;
use crate::mim_types::{deflate_index_load_gzip, DeflateIndex};
use anyhow::Result;
use lender::prelude::*;
// use needletail::errors::ParseError;
// use needletail::parser::{FastxReader, SequenceRecord};
use paraseq::fastx::{Reader, RecordSet};
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct ReadIter {
    // reader: Box<dyn FastxReader + 'a>,
    reader: Reader<GzipStreamReader>,
    record_set: RecordSet,
    niter: usize,
}

impl ReadIter {
    pub fn new(reader: Reader<GzipStreamReader>, niter: usize) -> Self {
        let record_set = reader.new_record_set();
        Self {
            reader,
            niter,
            record_set,
        }
    }
}

impl<'lend> Lending<'lend> for ReadIter {
    // type Lend = Result<SequenceRecord<'lend>, ParseError>;
    type Lend = Result<paraseq::fastx::RefRecord<'lend>>;
}

#[allow(clippy::should_implement_trait)]
impl Lender for ReadIter {
    fn next(&mut self) -> Option<Lend<'_, Self>> {
        if self.niter > 0 {
            self.niter -= 1;
            // FIXME: We want to store both a (pinned?) record set and an iterator over it.
            self.record_set.iter()
        } else {
            None
        }
    }
}

pub struct MultiParser {
    pub nworker: usize,
    pub fpath: PathBuf,
    pub ipath: PathBuf,
    pub chunk_assignments: Vec<Range<usize>>,
    pub index: DeflateIndex,
}

unsafe impl Send for MultiParser {}

/// Distributes x chunks evenly across t threads.
/// Returns a vector of ranges where each range represents the chunk indices
/// assigned to a thread (start inclusive, end exclusive).
fn distribute_chunks(x: usize, t: usize) -> Vec<Range<usize>> {
    if t == 0 {
        return vec![];
    }

    let chunks_per_thread = x / t;
    let remainder = x % t;

    let mut ranges = Vec::with_capacity(t);
    let mut start = 0;

    for i in 0..t {
        // First 'remainder' threads get an extra chunk
        let size = if i < remainder {
            chunks_per_thread + 1
        } else {
            chunks_per_thread
        };

        let end = start + size;
        ranges.push(start..end);
        start = end;
    }

    ranges
}

impl MultiParser {
    pub fn new_with_workers<P: AsRef<Path>>(fpath: P, ipath: P, nworker: usize) -> Self {
        let file = File::open(ipath.as_ref()).expect("File failed to open index file");
        let index = deflate_index_load_gzip(file).expect("failed to load index");

        let chunk_assignments = distribute_chunks(index.num_record_chunks as usize, nworker);

        Self {
            nworker,
            fpath: PathBuf::from(fpath.as_ref()),
            ipath: PathBuf::from(ipath.as_ref()),
            chunk_assignments,
            index,
        }
    }

    pub fn get_worker_stream(&self, worker_id: usize) -> Result<(usize, GzipStreamReader)> {
        if worker_id < self.nworker {
            let chunk_range = self.chunk_assignments[worker_id].clone();
            let mut gzfq =
                GzipStreamReader::open_at_checkpoint(&self.fpath, &self.index, chunk_range.start)?;

            let first_record_rank =
                self.index.record_boundaries[chunk_range.start].first_record_in_chunk;
            let record_offset = self.index.record_boundaries[chunk_range.start].byte_offset;
            if record_offset > gzfq.uncompressed_offset() {
                // discard the requisite number of bytes
                let mut discard_buf =
                    vec![0_u8; (record_offset - gzfq.uncompressed_offset()) as usize];
                gzfq.read_exact(&mut discard_buf)?;
            }

            let niter = if chunk_range.end >= self.index.record_boundaries.len() {
                self.index.total_record_count as u64 - first_record_rank
            } else {
                let last_record_rank =
                    self.index.record_boundaries[chunk_range.end].first_record_in_chunk;
                last_record_rank - first_record_rank
            };

            Ok((niter as usize, gzfq))
        } else {
            anyhow::bail!(
                "Requested work for worker {}, but only {} workers were registered.",
                worker_id,
                self.nworker
            )
        }
    }

    pub fn get_worker_iter<'a>(&'a self, worker_id: usize) -> Result<ReadIter> {
        let (niter, stream) = self.get_worker_stream(worker_id)?;
        let fastx_reader = paraseq::fastx::Reader::new(stream).unwrap();
        // let fastx_reader = needletail::parse_fastx_reader(stream).expect("invalid reader");

        Ok(ReadIter::new(fastx_reader, niter))
    }
}

/*
let file = File::open(&args.index_path).expect("File failed to index");
    let index = deflate_index_load_gzip(file).expect("failed to load index");
    assert!(
        args.checkpoint < index.list.len(),
        "requested checkpoint {} >= number of checkpoints {}",
        args.checkpoint,
        index.list.len()
    );

    eprintln!("opening at checkpoint: {}", args.checkpoint);
    let mut gzfq = GzipStreamReader::open_at_checkpoint(&args.fastq_path, &index, args.checkpoint)
        .expect("valid gzip stream reader");

    let record_offset = index.record_boundaries[args.checkpoint].byte_offset;
    if record_offset > gzfq.uncompressed_offset() {
        // discard the requisite number of bytes
        let mut discard_buf = vec![0_u8; (record_offset - gzfq.uncompressed_offset()) as usize];
        gzfq.read_exact(&mut discard_buf)?;
    }

    let mut fastx_reader = needletail::parse_fastx_reader(gzfq).expect("invalid reader");
*/
