//! # Mim
//!
//! `mim` allows multithreaded reading of large `.gz` files by using a auxiliary `.gz.mim` index.
//! This index stores 'checkpoints' of size ~32 kB every 32 MB, and allows decompression
//! to start anywhere within the file.
//! Additionally, we make this FASTA/FASTQ aware by storing the index and offset of the first record after each checkpoint.
//!
//! ### Building the index
//!
//! Given a `records.fastq.gz`, build the index using [`build_mim_index`].:
//! ```
//! use std::path::Path;
//! // Make checkpoints every 32 MiB.
//! let chunk_size = 32 * 1024 * 1024;
//! // Optional additional metadata to include.
//! let metadata: Option<serde_json::Value> = None;
//! mim::build_mim_index(Path::new("records.fastq.gz"), chunk_size, metadata, Some(Path::new("records.fastq.gz.mim")));
//! ```
//!
//! ### Using the index
//!
//! The main entrypoint for using the index is [`mim_reader`], followed by calling [`MimReader::get_reader`] inside each thread.
//! ```no_run
//! // Count the total length of the records in records.fastq.gz using 8 threads.
//! let gz_path = std::path::Path::new("records.fastq.gz");
//! let num_workers = 8;
//! // Requires `records.fastq.gz.mim` to exist.
//! let mim_reader: &mim::MimReader = &mim::mim_reader(&gz_path, num_workers);
//!
//! let total_bytes = std::sync::atomic::AtomicUsize::new(0);
//! std::thread::scope(|s| {
//!     for i in 0..num_workers {
//!         let total_bytes = &total_bytes;
//!         s.spawn(move || {
//!             // Pass the `reader` over bytes to needletail for parsing.
//!             let reader = mim_reader.get_reader(i).unwrap();
//!             let mut parser = needletail::parse_fastx_reader(reader).unwrap();
//!
//!             // Alternatively, for thread i:
//!             // let mut parser = mim_reader.get_needletail_parser(i).unwrap();
//!
//!             // Count bytes.
//!             let mut num_bytes = 0;
//!             while let Some(record) = parser.next() {
//!                 let record = record.unwrap();
//!                 num_bytes += record.seq().len();
//!             }
//!             total_bytes.fetch_add(num_bytes, std::sync::atomic::Ordering::Relaxed);
//!         });
//!     }
//! });
//! eprintln!("Total bytes: {}", total_bytes.into_inner());
//! ```
//!
//! With the `needletail` feature flag, these convenience functions are provided:
//! - [`MimReader::get_needletail_parser`], which returns a [`needletail::FastxReader`] over the records in each chunk.
//! - [`MimReader::get_needletail_iter`], which returns a lending iterator over the [`needletail::parser::SequenceRecord`] records in each chunk.
//!
//! With the `paraseq` feature flag, [`MimReader`] implements [`paraseq::prelude::ParallelReader`] for use with [`paraseq::prelude::ParallelProcessor`].
//!
//! ### Paired-end and multiple files
//!
//! Paired-end and synchronous multi-file processing is provided by [`MultiMimReader`].
//! This takes multiple `.gz` files and needs a `.mim` for each, and returns readers that work in lock-step:
//! the first file is split into `num_workers` roughly equally sized chunks,
//! and the corresponding starting records are found in each of the other files.
//!
//! ### CLI
//!
//! See the github readme for details.
//!
//! - `mim index` builds the `.mim` index.
//! - `mim unzip` unzips a `.gz` file using the `.mim` index.
//! - `mim unzip --parts 8` unzips a `.gz` file into 8 record-aligned parts `input.fastq.gz.<PART>`.
//! - `mim unzip --parts 8 --pipe` creates 8 named single-use pipes that can be read by another program.

mod indexer;
#[cfg(feature = "paraseq")]
mod paraseq_processor;
mod reader;
mod record_counter;

pub mod gzip_reader;
pub mod types;

use std::path::Path;

pub use indexer::build_mim_index;
pub use reader::{MimReader, MultiMimReader, ReadIter, hash_gz_file};

/// Read the `.mim` file corresponding to the given `.gz` and initialize it for `num_workers`.
pub fn mim_reader(gz_path: &Path, num_workers: usize) -> MimReader {
    MimReader::new(gz_path, num_workers)
}

/// Read the given `.mim` file for the `.gz` and initialize it for `num_workers`.
pub fn mim_reader_with_index(gz_path: &Path, index_path: &Path, num_workers: usize) -> MimReader {
    MimReader::new_with_index(gz_path, index_path, num_workers)
}
