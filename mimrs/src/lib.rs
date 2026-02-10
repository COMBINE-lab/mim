mod indexer;
#[cfg(feature = "paraseq")]
mod paraseq_processor;
mod reader;
mod record_counter;

pub mod gzip_reader;
pub mod types;

use std::path::Path;

pub use indexer::build_mim_index;
pub use reader::{MimReader, MultiMimReader, ReadIter};

/// Read the `.mim` file corresponding to the given `.gz` and initialize it for `num_workers`.
pub fn mim_reader(gz_path: &Path, num_workers: usize) -> MimReader {
    MimReader::new(gz_path, num_workers)
}

/// Read the given `.mim` file for the `.gz` and initialize it for `num_workers`.
pub fn mim_reader_with_index(gz_path: &Path, index_path: &Path, num_workers: usize) -> MimReader {
    MimReader::new_with_index(gz_path, index_path, num_workers)
}
