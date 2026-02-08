//! This file is based on zran C code which as transpiled to Rust by Claude.

use crate::mim_types::{
    Blake3Hash, DecompressionMode, DeflateCheckPoint, MimIndex, RecordCheckpoint,
    MIMINDEX_FILE_CONSTANT,
};
use crate::record_counter;
//use libz_ng_sys::z_stream;
//use libz_ng_sys::{self as zlib, Z_OK};

use libz_rs_sys::{self as zlib};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::ptr;
use tracing::{debug, info, trace};

/// Buffer size for the input file.
const INPUT_BUF_SIZE: usize = 1024 * 1024;
/// Decompress up to 128kB of output at a time.
/// 
/// We only ever read the last `WINSIZE` bytes of this,
/// but longer decompression loops are faster.
const OUTPUT_BUF_SIZE: usize = 256 * 1024;
/// Context window size for the index.
const WINSIZE: usize = 32 * 1024;

/// Metadata structure that is CBOR json-encoded.
///
/// This allows for backwards-compatible extensions.
#[derive(Debug, Serialize, Deserialize)]
struct IndexMetadata {
    user_metadata: Option<JsonValue>,
}

impl MimIndex {
    fn new() -> Self {
        Self {
            version: 0,
            metadata: Vec::new(),
            num_checkpoints: 0,
            mode: crate::mim_types::DecompressionMode::NONE,
            plain_size: 0,
            checkpoints: Vec::new(),
            record_boundaries: Vec::new(),
            num_record_chunks: 0,
            total_num_records: 0,
            plain_hash: Blake3Hash::default(),
        }
    }
}

/// Error types for index operations
#[derive(Debug)]
pub enum IndexError {
    Io(io::Error),
    Compression(String),
    OutOfMemory,
    PrematureEnd,
    DataError,
    InvalidHeader,
    ZlibError(i32),
}

impl From<io::Error> for IndexError {
    fn from(err: io::Error) -> Self {
        IndexError::Io(err)
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Io(e) => write!(f, "I/O error: {}", e),
            IndexError::Compression(s) => write!(f, "Compression error: {}", s),
            IndexError::OutOfMemory => write!(f, "Out of memory"),
            IndexError::PrematureEnd => write!(f, "Building index ended prematurely"),
            IndexError::DataError => write!(f, "Compressed data error"),
            IndexError::InvalidHeader => write!(f, "Invalid header"),
            IndexError::ZlibError(code) => write!(f, "Zlib error code: {}", code),
        }
    }
}

impl std::error::Error for IndexError {}

/// Add an access point to the index
fn add_point(
    index: &mut MimIndex,
    gz_pos: i64,
    plain_pos: i64,
    gzip_member_start_pos: i64,
    output_ringbuf: &[u8; OUTPUT_BUF_SIZE],
    strm: &zlib::z_stream,
    chunk_size: i64,
) -> Result<(), IndexError> {
    // Context window does not go before the start of the gzip member.
    let window_size = (plain_pos - gzip_member_start_pos).min(WINSIZE as i64) as usize;

    let mut window = vec![0u8; window_size];
    {
        // 'unroll' the `output_ringbuf` into `window`.
        // The last `strm.avail_out` bytes are the oldest, and the ones before are new.
        let recent = OUTPUT_BUF_SIZE - strm.avail_out as usize;
        let prefix_copy = recent.min(window_size);
        window[window_size - prefix_copy..]
            .copy_from_slice(&output_ringbuf[recent - prefix_copy..recent]);
        // Take the rest from the suffix.
        let suffix_copy = window_size - prefix_copy;
        window[..suffix_copy]
            .copy_from_slice(&output_ringbuf[OUTPUT_BUF_SIZE - suffix_copy..OUTPUT_BUF_SIZE]);
    }

    // TODO:??
    let bits = (strm.data_type & 7) as u8;

    index.checkpoints.push(DeflateCheckPoint {
        plain_pos,
        gz_pos,
        bits,
        window_size: window_size as u32,
        window,
    });

    index.num_checkpoints += 1;

    trace!(
            "adding access point {} at {} (read) {} (written); distance {} vs chunk size {}  | window {window_size}",
            index.num_checkpoints,
            gz_pos,
            plain_pos,
            plain_pos
                - (if index.num_checkpoints > 1 {
                    index.checkpoints[index.num_checkpoints as usize - 2].plain_pos
                } else {
                    0
                }),
            chunk_size
        );

    Ok(())
}

struct DecompressionState {
    strm: zlib::z_stream,
    mode: DecompressionMode,

    /// Ringbuffer for decompression.
    output_ringbuf: [u8; OUTPUT_BUF_SIZE],

    /// Total number of bytes read from the input gz stream.
    in_pos: i64,
    /// Total number of decompressed plaintext bytes returned.
    plain_bytes_out: i64,
    /// Plaintext position of last checkpoint.
    last_checkpoint_pos: i64,
    /// Plaintext start position of current gzip member.
    gzip_member_start: i64,
}

impl DecompressionState {
    fn new() -> Self {
        let mut strm: zlib::z_stream = unsafe { std::mem::zeroed() };
        strm.zalloc = None;
        strm.zfree = None;
        strm.opaque = ptr::null_mut();
        strm.avail_in = 0;
        strm.next_in = ptr::null_mut();

        Self {
            strm,
            in_pos: 0,
            plain_bytes_out: 0,
            mode: DecompressionMode::NONE,
            last_checkpoint_pos: 0,
            output_ringbuf: [0u8; OUTPUT_BUF_SIZE],
            gzip_member_start: 0,
        }
    }
}

/// Handle the boundary between concatenated gzip members.
///
/// Must only be called if there is more data available.
fn handle_gzip_member_boundary(state: &mut DecompressionState, ret: &mut i32) {
    if *ret == zlib::Z_STREAM_END && state.mode == DecompressionMode::GZIP {
        trace!(
            "Z_STREAM_END detected: avail_in={}, beg={}, totout={}",
            state.strm.avail_in,
            state.gzip_member_start,
            state.plain_bytes_out
        );

        // There is more input after the end of a gzip member
        // Reset the inflate state to read another gzip member
        // On success, this sets ret back to Z_OK to continue decompressing
        *ret = unsafe { zlib::inflateReset2(&mut state.strm, DecompressionMode::GZIP as i32) };
        trace!("Called inflateReset2, ret={}", ret);

        if *ret == zlib::Z_OK {
            state.gzip_member_start = state.plain_bytes_out; // Reset history
            trace!("Reset beg to {}", state.gzip_member_start);
        }
    }
}

/// Build a deflate index from a gzip file using raw zlib API
fn deflate_index_build<R: BufRead>(reader: &mut R, chunk_size: i64) -> Result<MimIndex, IndexError> {
    let mut hasher = blake3::Hasher::new();
    let mut index = MimIndex::new();
    let mut state = DecompressionState::new();

    let mut record_counter = record_counter::RecordCounter::new();

    let mut ret: i32 = zlib::Z_OK;

    let mut num_records = 0;

    let mut input_buf;

    // Main decompression loop
    'main_loop: loop {

        if state.strm.avail_in > 0 {
            // If the last loop reached end-of-stream and there is more data, start a new member.
            handle_gzip_member_boundary(&mut state, &mut ret);
            if ret != zlib::Z_OK {
                break 'main_loop;
            }
        } else {
            input_buf = reader.fill_buf()?;
            let bytes_read = input_buf.len();

            // NOTE: We assume that we won't receive additional data after 0 has been returned.
            // (Eg if the underlying file is appended to.)
            if bytes_read == 0 {
                break 'main_loop;
            }

            // If the last loop reached end-of-stream and there is more data, start a new member.
            // FIXME: Does `reset_inflate` touch `avail_in`?
            handle_gzip_member_boundary(&mut state, &mut ret);
            if ret != zlib::Z_OK {
                break 'main_loop;
            }

            // FIXME: This can also trigger randomly, not only at start of file?
            // And gzip headers can occur also in the middle of file reads?
            if let [0x1f, 0x8b, ..] = input_buf{
                trace!(
                    "Read new gzip header at totin={}, totout={}, checkpoint={}",
                    state.in_pos,
                    state.plain_bytes_out,
                    index.num_checkpoints
                );
            }
            // trace!("Read {} bytes", bytes_read);

            // Hash the gzip file itself.
            hasher.update(input_buf);

            state.strm.avail_in = bytes_read as u32;
            state.strm.next_in = input_buf.as_ptr();

            // FIXME: Can the mode change between gzip members?
            if state.mode == DecompressionMode::NONE && bytes_read > 0 {
                state.mode = match input_buf[0] {
                    b if b & 0x0f == 8 => DecompressionMode::ZLIB,
                    0x1f => DecompressionMode::GZIP,
                    _ => DecompressionMode::RAW,
                };

                ret = unsafe {
                    zlib::inflateInit2_(
                        &mut state.strm,
                        state.mode as i32,
                        zlib::zlibVersion(),
                        std::mem::size_of::<zlib::z_stream>() as i32,
                    )
                };
                if ret != zlib::Z_OK {
                    break 'main_loop;
                }
            }
        }

        // Wrap around the ring buffer.
        if state.strm.avail_out == 0 {
            state.strm.avail_out = OUTPUT_BUF_SIZE as u32;
            state.strm.next_out = state.output_ringbuf.as_mut_ptr();
        }

        // Handle special case for RAW mode at the start
        if state.mode == DecompressionMode::RAW && index.num_checkpoints == 0 {
            state.strm.data_type = 0x80;
        } else {
            let out_before = state.strm.avail_out as i64;
            let in_before = state.strm.avail_in as i64;
            ret = unsafe { zlib::inflate(&mut state.strm, zlib::Z_BLOCK) };
            let out_after = state.strm.avail_out as i64;
            let in_after = state.strm.avail_in as i64;
            let consumed = in_before - in_after;
            state.in_pos += consumed;
            reader.consume(consumed as usize);
            let produced = out_before - out_after;
            // trace!("Produced {} bytes", produced);
            state.plain_bytes_out += produced;
            // NOTE: Tracking: https://github.com/trifectatechfoundation/zlib-rs/issues/439
            // this *does not* seem to be necessary any longer, but I'm keeping it here just for
            // now.
            // Handle Z_DATA_ERROR that occurs at gzip member boundaries
            // When using Z_BLOCK mode with concatenated gzip files, inflate() can return
            // Z_DATA_ERROR when it encounters the length check at the end of a member,
            // especially if the member boundary doesn't align with a block boundary.
            // If we produced no output and we're in GZIP mode, treat this as Z_STREAM_END
            // to allow processing of the next member.

            if ret == zlib::Z_DATA_ERROR && state.mode == DecompressionMode::GZIP && produced == 0 {
                // This is likely a member boundary issue, treat as end of stream
                ret = zlib::Z_STREAM_END;
                trace!("HERE");
            }
            // FIXME: Do something with the record counting.
            let first_record_offset;
            (num_records, first_record_offset) = record_counter.push_bytes(
                &state.output_ringbuf
                    [OUTPUT_BUF_SIZE - out_before as usize..OUTPUT_BUF_SIZE - out_after as usize],
            );
            if let Some(last) = index.record_boundaries.last_mut() {
                if last.next_record_pos == u64::MAX && let Some(fr) = first_record_offset {
                    last.next_record_pos = fr as u64;
                    trace!("Updated next_record_pos={}", fr);
                }
            }
        }

        // Check if we should add an access point
        // FIXME: Doesn't this create a checkpoint right after the first read bytes of the file?
        // FIXME: What does ==0x80 mean? => The end of a gzip block. Is this ever not true?
        // What are the implications of blocks being 32kB? (are they?)
        if (state.strm.data_type & 0xc0) == 0x80
            && (index.num_checkpoints == 0
                || state.plain_bytes_out - state.last_checkpoint_pos >= chunk_size)
        {
            add_point(
                &mut index,
                state.in_pos,
                state.plain_bytes_out,
                state.gzip_member_start,
                &state.output_ringbuf,
                &state.strm,
                chunk_size,
            )?;
            debug!(
                "Added checkpoint at totin={}, totout={}, checkpoint={}",
                state.in_pos, state.plain_bytes_out, index.num_checkpoints
            );
            trace!("Num records in this chunk: {}", num_records);
            index.record_boundaries.push(RecordCheckpoint {
                next_record_idx: num_records as u64,
                next_record_pos: u64::MAX,
            });
            state.last_checkpoint_pos = state.plain_bytes_out;
        }
    }

    index.record_boundaries.push(RecordCheckpoint {
        next_record_idx: num_records as u64,
        next_record_pos: index.plain_size as u64,
    });

    unsafe { zlib::inflateEnd(&mut state.strm) };

    if ret != zlib::Z_STREAM_END {
        return match ret {
            zlib::Z_NEED_DICT => Err(IndexError::DataError),
            zlib::Z_MEM_ERROR => Err(IndexError::OutOfMemory),
            zlib::Z_BUF_ERROR => Err(IndexError::PrematureEnd),
            _ => Err(IndexError::ZlibError(ret)),
        };
    }

    // Finalize hash
    let hash = hasher.finalize();
    index.plain_hash.copy_from_slice(hash.as_bytes());

    let mut msg = vec![0_u8; 128];
    write!(&mut msg, "BLAKE3 checksum:")?;
    for byte in hash.as_bytes() {
        write!(&mut msg, "{:02x}", byte)?;
    }
    info!("{}", str::from_utf8(&msg).expect("valid utf8"));

    index.mode = state.mode;
    index.plain_size = state.plain_bytes_out;

    Ok(index)
}

/// Build the `.mim` index for `gzip_file` at either `output_file` or `<gzip_file>.mim`.
pub fn build_index(
    gzip_file: &Path,
    chunk_size: i64,
    user_metadata: Option<JsonValue>,
    output_file: Option<&Path>,
) -> Result<(), IndexError> {
    trace!("Opening file: {:?}", gzip_file);
    let file = File::open(gzip_file)?;
    let mut buf_reader = BufReader::with_capacity(INPUT_BUF_SIZE, file);

    trace!("Building deflate index...");
    let mut index = deflate_index_build(&mut buf_reader, chunk_size)?;

    info!(
        "zran: built index with {} access points!",
        index.num_checkpoints
    );

    index.version = 0;

    // Create metadata
    let metadata = IndexMetadata { user_metadata };
    index.metadata =
        serde_cbor::to_vec(&metadata).map_err(|e| IndexError::Compression(e.to_string()))?;

    // Process FASTQ records
    trace!("Getting record boundaries from FASTQ file");

    index.num_record_chunks = index.record_boundaries.len() as i64 - 1;
    index.total_num_records = index.record_boundaries.last().unwrap().next_record_idx as _;
    info!("Got {} records from FASTQ file.", index.total_num_records);

    // Save index
    let output_path = match output_file {
        Some(path) => path.to_owned(),
        None => gzip_file.with_added_extension("mim"),
    };

    trace!(
        "zran: attempting to write index to {}",
        output_path.to_string_lossy()
    );
    write_mim_index(&output_path, &index)?;

    info!(
        "zran: wrote index with {} access points to {}",
        index.num_checkpoints,
        output_path.to_string_lossy()
    );

    Ok(())
}

/// Write index to a gzipped file.
fn write_mim_index(path: &Path, index: &MimIndex) -> Result<(), IndexError> {
    let writer = File::create(path)?;
    let buffered_writer = io::BufWriter::new(writer);
    // TODO: Use LZ4 instead?
    let mut gz_writer =
        flate2::write::GzEncoder::new(buffered_writer, flate2::Compression::default());
    gz_writer.write_all(MIMINDEX_FILE_CONSTANT)?;
    bincode::encode_into_std_write(index, &mut gz_writer, bincode::config::legacy())
        .map_err(|e| IndexError::Compression(format!("Failed to encode index: {}", e)))?;
    Ok(())
}
