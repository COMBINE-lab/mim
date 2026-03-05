//! Type definitions and IO for the main [`MimIndex`] type.
use std::{
    fs::File,
    io::{BufWriter, Error, ErrorKind, Read, Result},
    ops::Range,
    path::Path,
};

use tracing::{debug, trace};

/// Magic file signature constant that is written at the start of each .mim file.
/// <https://en.wikipedia.org/wiki/List_of_file_signatures>
pub const MIMINDEX_FILE_CONSTANT: &[u8; 8] = b"MIMINDEX";

/// Wrapper type for Blake3Hash.
pub type Blake3Hash = [u8; 32];

/// Checkpoint storing information about an arbitrary position in a .gz file.
/// Contains the preceding 32KiB window to enable decompression, as well as
/// the exact (byte and bit) offset in the decompressed file that this checkpoints corresponds to.
#[derive(Clone, Debug, bincode::Encode, bincode::Decode)]
pub struct CheckPoint {
    // Deflate data.
    /// Byte offset in the compressed file where this chunk starts.
    pub in_pos: i64,
    /// Number of spare bits in the compressed stream.
    pub bits: u8,
    /// Byte offset in the decompressed data where this chunk starts.
    pub out_pos: i64,
    /// The preceding 32KiB (or less) window for deflate state.
    pub window: Vec<u8>,

    // Record data.
    /// The 0-based index of the first record starting at or after this checkpoint.
    pub next_record_idx: u64,
    /// The offset in the full uncompressed data where the first record in this chunk starts.
    pub next_record_pos: u64,
}

/// The mode of the file. RAW for DEFLATE stream, or GZIP or ZLIB header.
#[derive(Default, Debug, bincode::Encode, bincode::Decode, PartialEq, Eq, Clone, Copy)]
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

    /// Blake3 hash of the .gz file.
    /// Stored early so it can be scanned quickly.
    pub input_hash: Blake3Hash,

    /// Whether the file is gzip, zlib, or raw DEFLATE.
    pub mode: DecompressionMode,

    /// Total size in bytes of the decompressed gzip data.
    pub output_size: i64,
    /// Total number of records.
    pub total_num_records: i64,

    /// CBOR serialized json string.
    pub metadata: Vec<u8>, // CBOR blob (deserialized)

    /// The checkpoints. Most of the size is here.
    /// Also contains a past-the-end entry.
    pub checkpoints: Vec<CheckPoint>,
}

impl MimIndex {
    /// Decompress and then deserialize a .mim file.
    pub fn read_path(path: &Path) -> Result<MimIndex> {
        let reader = File::open(path)?;
        Self::read_reader(reader)
    }

    /// Decompress and then deserialize a .mim file.
    pub fn read_reader(reader: impl std::io::Read) -> Result<MimIndex> {
        let mut reader = std::io::BufReader::with_capacity(256 * 1024, reader);
        {
            // Check file constant.
            let mut file_signature = [0; 8];
            reader.read_exact(&mut file_signature)?;
            if file_signature != *MIMINDEX_FILE_CONSTANT {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "File signature does not match MIMINDEX constant.",
                ));
            }
        }
        // Here, the checkpoints and record_boundaries arrays are zeroed out.
        let mut index: MimIndex =
            bincode::decode_from_std_read(&mut reader, bincode::config::legacy())
                .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;

        // Now read the last two fields, which are gzipped.
        let mut gz_reader = flate2::bufread::GzDecoder::new(reader);
        index.checkpoints =
            bincode::decode_from_std_read(&mut gz_reader, bincode::config::legacy())
                .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        Ok(index)
    }

    /// Read only the Blake3 hash from a .mim file, without decompressing or deserializing the whole file.
    pub fn read_hash_from_mim_file(path: &Path) -> Result<Blake3Hash> {
        let reader = File::open(path)?;
        Self::read_hash_from_std_read(reader)
    }

    pub fn read_hash_from_std_read(
        reader: impl std::io::Read,
    ) -> std::result::Result<[u8; 32], Error> {
        let mut reader = std::io::BufReader::with_capacity(256, reader);
        {
            // Check file constant.
            let mut file_signature = [0; 8];
            reader.read_exact(&mut file_signature)?;
            if file_signature != *MIMINDEX_FILE_CONSTANT {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "File signature does not match MIMINDEX constant.",
                ));
            }
        }
        // Skip the version (8 bytes).
        let mut version_buf = [0; 8];
        reader.read_exact(&mut version_buf)?;
        assert_eq!(
            u64::from_le_bytes(version_buf),
            0,
            "Unsupported MIMINDEX version."
        );
        // Read the next 32 bytes for the hash.
        let mut input_hash = [0; 32];
        reader.read_exact(&mut input_hash)?;
        debug!("Read input hash {:?} from .mim file.", input_hash);
        Ok(input_hash)
    }

    pub fn verify_hash(&self, gz_path: &Path) -> bool {
        let hash = hash_gz_file(gz_path);
        hash == self.input_hash
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
            while i < self.checkpoints.len() - 1
                && self.checkpoints[i].next_record_pos < target_end as u64
            {
                i += 1;
            }
            ranges.push(start..i);
        }
        trace!("Chunk assignments: {ranges:?}");
        ranges
    }
}

/// Compute the black3 hash of a `.gz` file.
pub fn hash_gz_file(gz_path: &Path) -> [u8; 32] {
    debug!("hashing input file");
    let mut hasher = blake3::Hasher::new();
    let mut reader = std::fs::File::open(gz_path).expect("could not open gzip file for hashing");
    std::io::copy(
        &mut reader,
        &mut BufWriter::with_capacity(256 * 1024, &mut hasher),
    )
    .expect("could not hash gzip file");
    debug!("hashing done");
    let hash = *hasher.finalize().as_bytes();
    hash
}
