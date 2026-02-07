//! Type definitions for the main [`MimIndex`] type.
use std::{
    fs::File,
    io::{Error, ErrorKind, Read, Result},
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
    pub bits: u8,
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

#[derive(Default, bincode::Encode, bincode::Decode, PartialEq, Eq, Clone, Copy)]
pub enum DecompressionMode {
    #[default]
    NONE = 0,
    RAW = -15,
    ZLIB = 15,
    GZIP = 31,
}

/// Deflate index structure
#[derive(bincode::Encode, bincode::Decode)]
pub struct MimIndex {
    /// The version of the index.
    // TODO: Major/Minor/Patch versions?
    // Maybe 10000 * major + 100 * minor + patch?
    pub version: u64,
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
    pub mode: DecompressionMode,

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
