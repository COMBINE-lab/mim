//! Type definitions for the main [`MimIndex`] type.
use std::{
    fs::File,
    io::{Error, ErrorKind, Result},
    path::Path,
};

/// Magic file signature constant that is written at the start of each .mim file.
/// https://en.wikipedia.org/wiki/List_of_file_signatures
pub(crate) const MIMINDEX_FILE_CONSTANT: &[u8; 8] = b"MIMINDEX";

/// Wrapper type for Blake3Hash.
pub(crate) type Blake3Hash = [u8; 32];

/// Checkpoint storing information about an arbitrary position in a .gz file.
/// Contains the preceding 32KiB window to enable decompression, as well as
/// the exact (byte and bit) offset in the decompressed file that this checkpoints corresponds to.
#[derive(Clone, bincode::Encode, bincode::Decode)]
pub struct DeflateCheckPoint {
    /// Byte offset in the decompressed data where this chunk starts.
    pub plain_offset: i64,
    /// Byte offset in the compressed file where this chunk starts.
    pub gz_offset: i64,
    /// TODO: For which of the two streams is this? Can the other one also be unaligned?
    pub bits: i32,
    /// Number of bytes in the window that are used as a dictionary.
    pub dictionary_size: u32,
    /// The preceding 32KiB (or less) window for deflate state.
    pub window: Vec<u8>,
}

/// Information for getting the first record after a checkpoint.
#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct RecordCheckpoint {
    /// The 0-based index of the first record starting at or after this checkpoint.
    pub first_record_in_chunk: u64,
    /// The offset in the uncompressed data of this chunk where the first contained record starts.
    /// TODO: The first incomplete byte is always skipped.
    pub byte_offset: u64,
}

/// Deflate index structure
#[derive(bincode::Encode, bincode::Decode)]
pub struct MimIndex {
    /// CBOR serialized json string.
    pub metadata: Vec<u8>, // CBOR blob (deserialized)
    /// Number of checkpoints.
    // FIXME: drop for just checkpoints.len()?
    pub num_checkpoints: i32,
    // FIXME: drop for record_boundaries.len()?
    pub num_record_chunks: i64,
    /// Total size in bytes of the decompressed gzip data.
    pub plain_size: i64,
    /// Total number of records.
    pub total_num_records: i64,
    /// Blake3 hash of the plain decompressed data.
    pub plain_hash: Blake3Hash,

    /// FIXME: -15 for raw, 15 for zlib, 31 for gzip
    pub mode: i32,

    /// The decompression checkpoints. Most of the size is here.
    pub checkpoints: Vec<DeflateCheckPoint>,
    /// Byte offset and index of first record in each chunk.
    /// Also contains a past-the-end entry.
    pub record_boundaries: Vec<RecordCheckpoint>,
}

/// Decompress and then deserialize a .mim file.
pub fn read_mim_index(path: &Path) -> Result<MimIndex> {
    let reader = File::open(path)?;
    let buf_reader = std::io::BufReader::new(reader);
    let mut gz_reader = flate2::bufread::GzDecoder::new(buf_reader);
    bincode::decode_from_std_read(&mut gz_reader, bincode::config::legacy())
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))
}
