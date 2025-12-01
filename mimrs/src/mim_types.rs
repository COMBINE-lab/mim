use flate2::read::GzDecoder;
use serde_cbor::Value as CborValue;
use std::io::{Error, ErrorKind, Read, Result};

const BLAKE3_OUT_LEN: usize = 32;
const MIMINDEX_STR: &str = "MIMINDEX";

/// Access point structure
#[derive(Debug, Clone)]
pub struct Point {
    pub out: i64,        // offset in uncompressed data
    pub in_offset: i64,  // offset in compressed file of first full byte
    pub bits: i32,       // 0, or number of bits (1-7) from byte at in-1
    pub dict: u32,       // number of bytes in window to use as a dictionary
    pub window: Vec<u8>, // preceding 32K (or less) of uncompressed data
}

/// Information for getting the first record after an access point
#[derive(Default, Debug, Clone)]
pub struct RecordCheckpoint {
    pub first_record_in_chunk: u64,
    pub byte_offset: u64,
}

/// Deflate index structure
#[derive(Debug)]
pub struct DeflateIndex {
    pub metadata_dict: CborValue, // CBOR blob (deserialized)
    pub have: i32,                // number of access points
    pub mode: i32,                // -15 for raw, 15 for zlib, 31 for gzip
    pub length: i64,              // total length of uncompressed data
    pub list: Vec<Point>,         // list of access points
    pub record_boundaries: Vec<RecordCheckpoint>,
    pub num_record_chunks: i64,
    pub total_record_count: i64,
    pub compressed_hash: [u8; BLAKE3_OUT_LEN],
}

/// Read a scalar value from the reader
pub fn read_scalar<R: Read, T>(reader: &mut R) -> Result<T>
where
    T: Sized,
{
    let mut buffer = vec![0u8; std::mem::size_of::<T>()];
    reader.read_exact(&mut buffer)?;
    // SAFETY: We've allocated the correct size buffer and T is a plain old data type
    let value = unsafe { std::ptr::read(buffer.as_ptr() as *const T) };
    Ok(value)
}

/// Read a vector from the gzip file
pub fn read_vector<R: Read, T>(reader: &mut R) -> Result<Vec<T>>
where
    T: Sized + Clone + Default,
{
    // Read vector length
    let vec_len: usize = read_scalar(reader)?;

    // Read vector data
    let elem_size = std::mem::size_of::<T>();
    let total_bytes = elem_size * vec_len;
    let mut buffer = vec![0u8; total_bytes];
    reader.read_exact(&mut buffer)?;

    // Convert bytes to Vec<T>
    // SAFETY: We're reading POD types with correct size and alignment
    let mut vec = Vec::with_capacity(vec_len);
    unsafe {
        let ptr = buffer.as_ptr() as *const T;
        for i in 0..vec_len {
            vec.push(ptr.add(i).read());
        }
    }

    Ok(vec)
}

/// Deserialize deflate index from a gzip compressed file
pub fn deflate_index_load_gzip<R: Read>(reader: R) -> Result<DeflateIndex> {
    let mut gz = GzDecoder::new(reader);

    // read the magic header
    let mut magic_sig = vec![0u8; 8];
    gz.read_exact(&mut magic_sig)?;
    let magic_sig = str::from_utf8(&magic_sig).expect("signature is valid utf8");
    if magic_sig != MIMINDEX_STR {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected magic signature {MIMINDEX_STR}, but found {magic_sig}"),
        ));
    }

    // Read metadata_dict as CBOR
    let metadata_bytes: Vec<u8> = read_vector(&mut gz)?;
    let metadata_dict = serde_cbor::from_slice(&metadata_bytes)
        .map_err(|e| Error::new(ErrorKind::InvalidData, format!("CBOR parse error: {}", e)))?;

    // Read basic fields
    let mode: i32 = read_scalar(&mut gz)?;
    let have: i32 = read_scalar(&mut gz)?;
    let length: i64 = read_scalar(&mut gz)?;
    let num_record_chunks: i64 = read_scalar(&mut gz)?;

    // Read hash
    let mut compressed_hash = [0u8; BLAKE3_OUT_LEN];
    gz.read_exact(&mut compressed_hash)?;

    // Read access points
    let mut list = Vec::with_capacity(have as usize);
    for _ in 0..have {
        let out: i64 = read_scalar(&mut gz)?;
        let in_offset: i64 = read_scalar(&mut gz)?;
        let bits: i32 = read_scalar(&mut gz)?;
        let dict: u32 = read_scalar(&mut gz)?;

        // Read window data
        let mut window = vec![0u8; dict as usize];
        gz.read_exact(&mut window)?;

        list.push(Point {
            out,
            in_offset,
            bits,
            dict,
            window,
        });
    }

    // Read record boundaries
    let record_boundaries: Vec<RecordCheckpoint> = read_vector(&mut gz)?;

    // Read total record count
    let total_record_count: i64 = read_scalar(&mut gz)?;

    Ok(DeflateIndex {
        metadata_dict,
        have,
        mode,
        length,
        list,
        record_boundaries,
        num_record_chunks,
        total_record_count,
        compressed_hash,
    })
}
