mod indexer;
#[cfg(feature = "paraseq")]
mod paraseq_processor;
mod reader;
mod record_counter;

pub mod gzip_reader;
pub mod types;

pub use indexer::build_mim_index;
pub use reader::{MimReader, MultiPairParser, ReadIter};
