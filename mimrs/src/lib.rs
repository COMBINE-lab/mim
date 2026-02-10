pub mod gzip_reader;
mod indexer;
mod record_counter;

pub mod paraseq_reader;
mod reader;
pub mod types;

pub use indexer::build_mim_index;
pub use reader::{MimReader, MultiPairParser, ReadIter};
