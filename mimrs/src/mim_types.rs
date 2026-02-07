//! Type definitions for the main [`MimIndex`] type.
use flate2::read::GzDecoder;
use std::io::{Error, ErrorKind, Read, Result};

/// Magic file signature constant that is written at the start of each .mim file.
/// https://en.wikipedia.org/wiki/List_of_file_signatures
pub(crate) const MIMINDEX_FILE_CONSTANT: &[u8; 8] = b"MIMINDEX";

/// Wrapper type for Blake3Hash.
pub(crate) type Blake3Hash = [u8; 32];

/// Checkpoint storing information about an arbitrary position in a .gz file.
/// Contains the preceding 32KiB window to enable decompression, as well as
/// the exact (byte and bit) offset in the decompressed file that this checkpoints corresponds to.
#[derive(Debug, Clone)]
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
#[derive(Default, Debug, Clone)]
pub struct RecordCheckpoint {
    /// The 0-based index of the first record starting at or after this checkpoint.
    pub first_record_in_chunk: u64,
    /// The offset in the uncompressed data of this chunk where the first contained record starts.
    /// TODO: The first incomplete byte is always skipped.
    pub byte_offset: u64,
}

/// Deflate index structure
#[derive(Debug)]
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

/// Read a scalar value from a reader.
pub fn read_scalar<R: Read, T: Sized>(reader: &mut R) -> Result<T> {
    let value = std::mem::MaybeUninit::<T>::uninit();
    reader.read_exact(unsafe {
        std::slice::from_raw_parts_mut(value.as_ptr() as *mut u8, std::mem::size_of::<T>())
    })?;
    Ok(unsafe { value.assume_init().into() })
}

/// Read a vector from a reader, with a `u64` length.
pub fn read_vector<R: Read, T: Sized>(reader: &mut R) -> Result<Vec<T>> {
    let len: u64 = read_scalar(reader)?;
    (0..len)
        .map(|_| read_scalar(reader))
        .collect::<Result<Vec<T>>>()
}

/// Decompress and then deserialize a .mim file.
// FIXME: Replace by a derive-based approach using eg bincode.
pub fn deflate_index_load_gzip<R: Read>(reader: R) -> Result<MimIndex> {
    let mut gz = GzDecoder::new(reader);

    {
        // read the magic header
        let mut magic_sig = [0u8; 8];
        gz.read_exact(&mut magic_sig)?;
        if magic_sig != *MIMINDEX_FILE_CONSTANT {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Expected magic signature {MIMINDEX_FILE_CONSTANT:?} ({}), but found {magic_sig:?}",
                    str::from_utf8(MIMINDEX_FILE_CONSTANT).unwrap()
                ),
            ));
        }
    }

    // Read metadata_dict as CBOR
    let metadata: Vec<u8> = read_vector(&mut gz)?;

    // Read basic fields
    let mode: i32 = read_scalar(&mut gz)?;
    let num_checkpoints: i32 = read_scalar(&mut gz)?;
    let plain_size: i64 = read_scalar(&mut gz)?;
    let num_record_chunks: i64 = read_scalar(&mut gz)?;

    // Read hash
    let mut plain_hash = Blake3Hash::default();
    gz.read_exact(&mut plain_hash)?;

    // Read access points
    let mut checkpoints = Vec::with_capacity(num_checkpoints as usize);
    for _ in 0..num_checkpoints {
        let plain_offset: i64 = read_scalar(&mut gz)?;
        let gz_offset: i64 = read_scalar(&mut gz)?;
        let bits: i32 = read_scalar(&mut gz)?;
        let dictionary_size: u32 = read_scalar(&mut gz)?;

        // Read window data
        let mut window = vec![0u8; dictionary_size as usize];
        gz.read_exact(&mut window)?;

        checkpoints.push(DeflateCheckPoint {
            plain_offset,
            gz_offset,
            bits,
            dictionary_size,
            window,
        });
    }

    // Read record boundaries
    let record_boundaries: Vec<RecordCheckpoint> = read_vector(&mut gz)?;

    // Read total record count
    let total_num_records: i64 = read_scalar(&mut gz)?;

    Ok(MimIndex {
        metadata,
        num_checkpoints,
        mode,
        plain_size,
        checkpoints,
        record_boundaries,
        num_record_chunks,
        total_num_records,
        plain_hash,
    })
}
