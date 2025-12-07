use crate::gzip_reader::GzipStreamReader;
use crate::mim_types::{DeflateIndex, deflate_index_load_gzip};
use anyhow::Result;
use lender::prelude::*;
use needletail::errors::ParseError;
use needletail::parser::{FastxReader, SequenceRecord};
//use paraseq::fastx::{Reader, RecordSet};
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use tracing::trace;

pub struct ReadIter<'a> {
    reader: Box<dyn FastxReader + 'a>,
    //reader: Reader<GzipStreamReader>,
    // record_set: RecordSet,
    // idx: usize,
    niter: usize,
    //_phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> ReadIter<'a> {
    pub fn new(reader: Box<dyn FastxReader + 'a>, niter: usize) -> Self {
        Self { reader, niter }
    }
}

impl<'this, 'lend> Lending<'lend> for ReadIter<'this> {
    type Lend = Result<SequenceRecord<'lend>, ParseError>;
}

#[allow(clippy::should_implement_trait)]
impl<'this> Lender for ReadIter<'this> {
    fn next(&mut self) -> Option<Lend<'_, Self>> {
        if self.niter > 0 {
            self.niter -= 1;
            self.reader.next()
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

pub struct MultiPairParser {
    pub nworker: usize,
    pub fpaths: Vec<PathBuf>,
    pub ipaths: Vec<PathBuf>,
    /// chunk assignments are with respect to read1 files
    pub chunk_assignments: Vec<Range<usize>>,
    pub indexes: Vec<DeflateIndex>,
}

unsafe impl Send for MultiPairParser {}

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

    /// returns a reader that only reads a specific number of bytes
    pub fn get_worker_stream_by_bytes(
        &self,
        worker_id: usize,
    ) -> Result<(usize, std::io::Take<GzipStreamReader>)> {
        if worker_id < self.nworker {
            let chunk_range = self.chunk_assignments[worker_id].clone();
            let mut gzfq =
                GzipStreamReader::open_at_checkpoint(&self.fpath, &self.index, chunk_range.start)?;

            let _first_record_rank =
                self.index.record_boundaries[chunk_range.start].first_record_in_chunk;
            let record_offset = self.index.record_boundaries[chunk_range.start].byte_offset;
            if record_offset > gzfq.uncompressed_offset() {
                // discard the requisite number of bytes
                let mut discard_buf =
                    vec![0_u8; (record_offset - gzfq.uncompressed_offset()) as usize];
                gzfq.read_exact(&mut discard_buf)?;
            }

            let nbytes = if chunk_range.end >= self.index.record_boundaries.len() {
                self.index.length as u64 - record_offset
            } else {
                let last_record_offset = self.index.record_boundaries[chunk_range.end].byte_offset;
                last_record_offset - record_offset
            };

            Ok((nbytes as usize, gzfq.take(nbytes)))
        } else {
            anyhow::bail!(
                "Requested work for worker {}, but only {} workers were registered.",
                worker_id,
                self.nworker
            )
        }
    }

    pub fn get_needletail_parser_for_worker<'a>(
        &'a self,
        worker_id: usize,
    ) -> Result<Box<dyn FastxReader + 'a>, ParseError> {
        let (nbytes, byte_limited_stream) = self
            .get_worker_stream_by_bytes(worker_id)
            .unwrap_or_else(|_| {
                panic!("could not get byte-limited stream for workder {worker_id}")
            });
        trace!("Worker {worker_id} will yield {nbytes} total uncompressed bytes");
        //let fastx_reader = paraseq::fastx::Reader::new(stream).unwrap();
        needletail::parse_fastx_reader(byte_limited_stream)
    }

    pub fn get_worker_iter<'a>(&'a self, worker_id: usize) -> Result<ReadIter<'a>> {
        let (niter, stream) = self.get_worker_stream(worker_id)?;
        //let fastx_reader = paraseq::fastx::Reader::new(stream).unwrap();
        let fastx_reader = needletail::parse_fastx_reader(stream).expect("invalid reader");

        Ok(ReadIter {
            reader: fastx_reader,
            niter,
            //_phantom: std::marker::PhantomData,
        })
    }
}

impl MultiPairParser {
    pub fn new_with_workers<P: AsRef<Path>>(
        fpaths: &[P],
        ipaths: &[P],
        nworker: usize,
    ) -> anyhow::Result<Self> {
        /// for now, we only handle a single pair of reads
        assert_eq!(fpaths.len(), 2);
        assert_eq!(ipaths.len(), 2);

        let index_files: Vec<std::fs::File> = ipaths
            .iter()
            .map(|f| File::open(f.as_ref()).expect("Failed to open file"))
            .collect();
        let indexes: Vec<DeflateIndex> = index_files
            .iter()
            .map(|f| deflate_index_load_gzip(f).expect("failed to load index"))
            .collect();

        // distribute chunks based on the first file
        let chunk_assignments = distribute_chunks(
            indexes
                .first()
                .expect("at least two indexes")
                .num_record_chunks as usize,
            nworker,
        );

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
            // get the chunk in file 1
            let chunk_range = self.chunk_assignments[worker_id].clone();
            let mut gzfq = GzipStreamReader::open_at_checkpoint(
                &self.fpaths[0],
                &self.indexes[0],
                chunk_range.start,
            )?;

            let first_record_rank =
                self.indexes[0].record_boundaries[chunk_range.start].first_record_in_chunk;
            let record_offset = self.indexes[0].record_boundaries[chunk_range.start].byte_offset;
            // discard the requisite number of bytes
            if record_offset > gzfq.uncompressed_offset() {
                let mut discard_buf =
                    vec![0_u8; (record_offset - gzfq.uncompressed_offset()) as usize];
                gzfq.read_exact(&mut discard_buf)?;
            }

            // the number of bytes the first reader will consume
            let nbytes = if chunk_range.end >= self.indexes[0].record_boundaries.len() {
                self.indexes[0].length as u64 - record_offset
            } else {
                let last_record_offset =
                    self.indexes[0].record_boundaries[chunk_range.end].byte_offset;
                last_record_offset - record_offset
            };

            // now sync the second file up with the first
            // find the chunk we jump to in file 2
            let mut second_file_chunk = self.indexes[1]
                .record_boundaries
                .partition_point(|x| x.first_record_in_chunk < first_record_rank);
            if self.indexes[1].record_boundaries[second_file_chunk].first_record_in_chunk
                > first_record_rank
            {
                second_file_chunk = 0_i64.max(second_file_chunk as i64 - 1) as usize;
            }
            // and the record rank starting this chunk
            let second_record_rank =
                self.indexes[1].record_boundaries[second_file_chunk].first_record_in_chunk;

            let mut gzfq2 = GzipStreamReader::open_at_checkpoint(
                &self.fpaths[1],
                &self.indexes[1],
                second_file_chunk,
            )?;
            // skip the byte sup to the first record
            let record_offset = self.indexes[1].record_boundaries[second_file_chunk].byte_offset;
            if record_offset > gzfq2.uncompressed_offset() {
                // discard the requisite number of bytes
                let mut discard_buf =
                    vec![0_u8; (record_offset - gzfq2.uncompressed_offset()) as usize];
                gzfq2.read_exact(&mut discard_buf)?;
            }

            let r1 = needletail::parse_fastx_reader(gzfq.take(nbytes))?;
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
