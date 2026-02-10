//! Type definitions for the main [`MimIndex`] type.
use std::{
    fs::File,
    io::{Error, ErrorKind, Read, Result},
    ops::Range,
    path::Path,
};

/// Magic file signature constant that is written at the start of each .mim file.
/// <https://en.wikipedia.org/wiki/List_of_file_signatures>
pub const MIMINDEX_FILE_CONSTANT: &[u8; 8] = b"MIMINDEX";

/// Wrapper type for Blake3Hash.
pub type Blake3Hash = [u8; 32];

/// Checkpoint storing information about an arbitrary position in a .gz file.
/// Contains the preceding 32KiB window to enable decompression, as well as
/// the exact (byte and bit) offset in the decompressed file that this checkpoints corresponds to.
#[derive(Clone, bincode::Encode, bincode::Decode)]
pub struct DeflateCheckPoint {
    /// Byte offset in the compressed file where this chunk starts.
    pub in_pos: i64,
    /// Number of spare bits in the compressed stream.
    pub bits: u8,
    /// Byte offset in the decompressed data where this chunk starts.
    pub out_pos: i64,
    /// The preceding 32KiB (or less) window for deflate state.
    pub window: Vec<u8>,
}

/// Information for getting the first record after a checkpoint.
#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct RecordCheckpoint {
    /// The 0-based index of the first record starting at or after this checkpoint.
    pub next_record_idx: u64,
    /// The offset in the full uncompressed data where the first record in this chunk starts.
    pub next_record_pos: u64,
}

/// The mode of the file. RAW for DEFLATE stream, or GZIP or ZLIB header.
#[derive(Default, bincode::Encode, bincode::Decode, PartialEq, Eq, Clone, Copy, Debug)]
pub enum DecompressionMode {
    #[default]
    NONE = 0,
    RAW = -15,
    ZLIB = 15,
    GZIP = 31,
}

/// The on-disk .mim data containing file metadata and checkpoints.
#[derive(bincode::Encode, bincode::Decode)]
pub struct MimIndex {
    /// The version of the index.
    // TODO: Major/Minor/Patch versions?
    // Maybe 10000 * major + 100 * minor + patch?
    pub version: u64,

    /// Whether the file is gzip, zlib, or raw DEFLATE.
    pub mode: DecompressionMode,
    /// Blake3 hash of the .gz file.
    pub input_hash: Blake3Hash,

    /// CBOR serialized json string.
    pub metadata: Vec<u8>, // CBOR blob (deserialized)

    /// Total size in bytes of the decompressed gzip data.
    pub output_size: i64,
    /// Total number of records.
    pub total_num_records: i64,

    /// The decompression checkpoints. Most of the size is here.
    pub checkpoints: Vec<DeflateCheckPoint>,
    /// Byte offset and index of first record in each chunk.
    /// Also contains a past-the-end entry.
    pub record_boundaries: Vec<RecordCheckpoint>,
}

impl MimIndex {
    /// Decompress and then deserialize a .mim file.
    pub fn read(path: &Path) -> Result<MimIndex> {
        let reader = File::open(path)?;
        let buf_reader = std::io::BufReader::with_capacity(256 * 1024, reader);
        let mut gz_reader = flate2::bufread::GzDecoder::new(buf_reader);
        {
            // Check file constant.
            let mut file_signature = [0; 8];
            gz_reader.read_exact(&mut file_signature)?;
            if file_signature != *MIMINDEX_FILE_CONSTANT {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "File signature does not match MIMINDEX constant.",
                ));
            }
        }
        bincode::decode_from_std_read(&mut gz_reader, bincode::config::legacy())
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))
    }

    /// Returns for each worker the range of checkpoints that it will process.
    ///
    /// This balances the number of fastx bytes per worker, rather than just the number of records.
    // TODO: This could possibly be optimized slightly by minimizing the length of the maximum chunk.
    //       Or, we could take into account the length of the decompressed data as well.
    pub fn distribute_chunks(&self, num_workers: usize) -> Vec<Range<usize>> {
        let total_bytes = self.output_size as usize;
        let target_size = total_bytes.div_ceil(num_workers);
        // For each worker, take the first chunk where it overshoots the target bytes.
        let mut ranges = Vec::with_capacity(num_workers);
        let mut i = 0;
        for worker_id in 0..num_workers {
            let target_end = target_size * (worker_id + 1);
            let start = i;
            while i < self.checkpoints.len()
                && self.record_boundaries[i].next_record_pos < target_end as u64
            {
                i += 1;
            }
            ranges.push(start..i);
        }
        ranges
    }
}
