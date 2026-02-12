//! This file is based on zran C code which as transpiled to Rust by Claude.

use crate::gzip_reader::ZStreamWrapper;
use crate::record_counter;
use crate::types::{Blake3Hash, CheckPoint, DecompressionMode, MIMINDEX_FILE_CONSTANT, MimIndex};
//use libz_ng_sys::z_stream;
//use libz_ng_sys::{self as zlib, Z_OK};

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
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
    user_metadata: Option<serde_json::Value>,
}

impl MimIndex {
    fn new() -> Self {
        Self {
            version: 0,
            metadata: Vec::new(),
            mode: crate::types::DecompressionMode::NONE,
            output_size: 0,
            checkpoints: Vec::new(),
            total_num_records: 0,
            input_hash: Blake3Hash::default(),
        }
    }
}

/// Error types for index operations
#[derive(Debug)]
pub enum IndexError {
    Io(io::Error),
    Compression(String),
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
        }
    }
}

impl std::error::Error for IndexError {}

/// Add an access point to the index
fn add_checkpoint(
    index: &mut MimIndex,
    in_pos: i64,
    out_pos: i64,
    gzip_member_start_pos: i64,
    output_ringbuf: &[u8; OUTPUT_BUF_SIZE],
    strm: &libz_rs_sys::z_stream,
    chunk_size: i64,
    next_record_idx: u64,
) -> Result<(), IndexError> {
    // Context window does not go before the start of the gzip member.
    let window_size = (out_pos - gzip_member_start_pos).min(WINSIZE as i64) as usize;

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

    let bits = (strm.data_type & 7) as u8;
    index.checkpoints.push(CheckPoint {
        out_pos,
        in_pos,
        bits,
        window,
        next_record_idx,
        next_record_pos: u64::MAX,
    });

    let idx = index.checkpoints.len() - 1;
    trace!(
        "adding access point {} at {} (read) {} (written); distance {} vs chunk size {}  | window {window_size}",
        idx,
        in_pos,
        out_pos,
        out_pos
            - (if idx > 0 {
                index.checkpoints[idx as usize - 1].out_pos
            } else {
                0
            }),
        chunk_size
    );

    Ok(())
}

struct DecompressionState {
    zstrm: ZStreamWrapper,
    file_mode: DecompressionMode,

    /// Ringbuffer for decompression.
    output_ringbuf: [u8; OUTPUT_BUF_SIZE],

    /// Total number of bytes read from the input gz stream.
    in_pos: i64,
    /// Total number of decompressed plaintext bytes returned.
    out_pos: i64,
    /// Plaintext position of last checkpoint.
    last_checkpoint_pos: i64,
    /// Plaintext start position of current gzip member.
    gzip_member_start: i64,
}

impl DecompressionState {
    fn new() -> Self {
        Self {
            zstrm: ZStreamWrapper::new(),
            in_pos: 0,
            out_pos: 0,
            file_mode: DecompressionMode::NONE,
            last_checkpoint_pos: 0,
            output_ringbuf: [0u8; OUTPUT_BUF_SIZE],
            gzip_member_start: 0,
        }
    }
}

/// Handle the boundary between concatenated gzip members.
///
/// Must only be called if there is more data available.
fn handle_gzip_member_boundary(state: &mut DecompressionState) -> Result<(), IndexError> {
    // Blocked GZIP is a thing, but blocked ZLIB and blocked DEFLATE are not.
    // (DEFLATE is only a single stream.)
    if state.file_mode == DecompressionMode::GZIP {
        trace!(
            "Z_STREAM_END detected: avail_in={}, beg={}, totout={}",
            state.zstrm.strm.avail_in, state.gzip_member_start, state.out_pos
        );

        // There is more input after the end of a gzip member
        // Reset the inflate state to read another gzip member
        // On success, this sets ret back to Z_OK to continue decompressing
        state.zstrm.reset(DecompressionMode::GZIP)?;
        state.gzip_member_start = state.out_pos;
    }
    Ok(())
}

/// Build a deflate index from a gzip file using raw zlib API
fn deflate_index_build<R: BufRead>(
    reader: &mut R,
    chunk_size: i64,
) -> Result<MimIndex, IndexError> {
    let mut hasher = blake3::Hasher::new();
    let mut index = MimIndex::new();
    let mut state = DecompressionState::new();

    let mut record_counter = record_counter::RecordCounter::new();

    let mut num_records = 0;

    // Loop over the input
    while let input_buf = reader.fill_buf()?
        && input_buf.len() > 0
    {
        state.zstrm.strm.next_in = input_buf.as_ptr();
        state.zstrm.strm.avail_in = input_buf.len() as u32;

        // At the start, set the decompression mode.
        if state.file_mode == DecompressionMode::NONE {
            // Detect file mode based on the first magic bytes.
            state.file_mode = match input_buf[0] {
                b if b & 0x0f == 8 => DecompressionMode::ZLIB,
                // gzip starts with 1F8B
                0x1f => DecompressionMode::GZIP,
                _ => DecompressionMode::RAW,
            };
            state.zstrm.init(state.file_mode)?;
        }

        // Hash the gzip file itself.
        hasher.update(input_buf);

        // Process the input buffer.
        while state.zstrm.strm.avail_in > 0 {
            // In RAW mode, force a checkpoint at the start.
            if state.file_mode == DecompressionMode::RAW && index.checkpoints.is_empty() {
                state.zstrm.strm.data_type = 0x80;
            } else {
                // Wrap around the output ring buffer.
                if state.zstrm.strm.avail_out == 0 {
                    state.zstrm.strm.avail_out = OUTPUT_BUF_SIZE as u32;
                    state.zstrm.strm.next_out = state.output_ringbuf.as_mut_ptr();
                }

                let (at_deflate_stream_end, consumed, produced) =
                    state.zstrm.inflate(libz_rs_sys::Z_BLOCK)?;

                if at_deflate_stream_end {
                    // If the last loop reached end-of-stream and there is more data, start a new member.
                    handle_gzip_member_boundary(&mut state)?;
                }

                trace!(
                    "STATE: {:>16b} consumed {}  produced {}",
                    state.zstrm.strm.data_type, consumed, produced
                );

                state.in_pos += consumed as i64;
                state.out_pos += produced as i64;

                // {
                //     // NOTE: Tracking: https://github.com/trifectatechfoundation/zlib-rs/issues/439
                //     // this *does not* seem to be necessary any longer, but I'm keeping it here just for
                //     // now.
                //     // Handle Z_DATA_ERROR that occurs at gzip member boundaries
                //     // When using Z_BLOCK mode with concatenated gzip files, inflate() can return
                //     // Z_DATA_ERROR when it encounters the length check at the end of a member,
                //     // especially if the member boundary doesn't align with a block boundary.
                //     // If we produced no output and we're in GZIP mode, treat this as Z_STREAM_END
                //     // to allow processing of the next member.
                //     if ret == libz_rs_sys::Z_DATA_ERROR
                //         && state.mode == DecompressionMode::GZIP
                //         && produced == 0
                //     {
                //         // This is likely a member boundary issue, treat as end of stream
                //         ret = libz_rs_sys::Z_STREAM_END;
                //         trace!("HERE");
                //     }
                // }

                let first_record_offset;
                (num_records, first_record_offset) = record_counter.push_bytes(
                    &state.output_ringbuf[OUTPUT_BUF_SIZE
                        - state.zstrm.strm.avail_out as usize
                        - produced as usize
                        ..OUTPUT_BUF_SIZE - state.zstrm.strm.avail_out as usize],
                );
                if let Some(last) = index.checkpoints.last_mut() {
                    if last.next_record_pos == u64::MAX
                        && let Some(fr) = first_record_offset
                    {
                        last.next_record_pos = fr as u64;
                        trace!("Updated next_record_pos={}", fr);
                    }
                }
            }

            // Check if we should add an access point
            // FIXME: Doesn't this create a checkpoint right after the first read bytes of the file?
            // FIXME: What does ==0x80 mean? => The end of a gzip block. Is this ever not true?
            // What are the implications of blocks being 32kB? (are they?)
            if (state.zstrm.strm.data_type & 0xc0) == 0x80
                && (index.checkpoints.is_empty()
                    || state.out_pos - state.last_checkpoint_pos >= chunk_size)
            {
                // If the previous checkpoint does not have a corresponding record start, drop it.
                if let Some(CheckPoint {
                    next_record_pos: u64::MAX,
                    ..
                }) = index.checkpoints.last_mut()
                {
                    debug!(
                        "Dropping checkpoint {} at out_pos={} with no record start",
                        index.checkpoints.len(),
                        state.out_pos
                    );
                    index.checkpoints.pop();
                }

                add_checkpoint(
                    &mut index,
                    state.in_pos,
                    state.out_pos,
                    state.gzip_member_start,
                    &state.output_ringbuf,
                    &state.zstrm.strm,
                    chunk_size,
                    num_records as u64,
                )?;
                debug!(
                    "Added checkpoint at totin={}, totout={}, checkpoint={}",
                    state.in_pos,
                    state.out_pos,
                    index.checkpoints.len()
                );
                trace!("Num records in this chunk: {}", num_records);
                state.last_checkpoint_pos = state.out_pos;
            }
        }
        let bytes_processed = input_buf.len();
        reader.consume(bytes_processed);
    }

    add_checkpoint(
        &mut index,
        state.in_pos,
        state.out_pos,
        state.gzip_member_start,
        &state.output_ringbuf,
        &state.zstrm.strm,
        chunk_size,
        num_records as u64,
    )?;
    index.checkpoints.last_mut().unwrap().next_record_pos = state.out_pos as u64;

    state.zstrm.end()?;

    // Finalize hash
    let hash = hasher.finalize();
    index.input_hash.copy_from_slice(hash.as_bytes());

    let mut msg = vec![0_u8; 128];
    write!(&mut msg, "BLAKE3 checksum:")?;
    for byte in hash.as_bytes() {
        write!(&mut msg, "{:02x}", byte)?;
    }
    info!("{}", str::from_utf8(&msg).expect("valid utf8"));

    index.mode = state.file_mode;
    index.output_size = state.out_pos;

    Ok(index)
}

/// Build and write the `.mim` index for the given `gzip_file`.
///
/// The output location defaults to `<gzip_file>.mim`.
pub fn build_mim_index(
    gzip_file: &Path,
    chunk_size: i64,
    user_metadata: Option<serde_json::Value>,
    index_path: Option<&Path>,
) -> Result<(), IndexError> {
    trace!("Opening file: {:?}", gzip_file);
    let file = File::open(gzip_file)?;
    let mut buf_reader = BufReader::with_capacity(INPUT_BUF_SIZE, file);

    trace!("Building deflate index...");
    let mut index = deflate_index_build(&mut buf_reader, chunk_size)?;

    info!(
        "zran: built index with {} access points!",
        index.checkpoints.len()
    );

    index.version = 0;

    // Create metadata
    let metadata = IndexMetadata { user_metadata };
    index.metadata =
        serde_cbor::to_vec(&metadata).map_err(|e| IndexError::Compression(e.to_string()))?;

    // Process FASTQ records
    trace!("Getting record boundaries from FASTQ file");

    index.total_num_records = index.checkpoints.last().unwrap().next_record_idx as _;
    info!("Got {} records from FASTQ file.", index.total_num_records);

    // Save index
    let output_path = super::default_index_path(gzip_file, index_path);

    trace!(
        "zran: attempting to write index to {}",
        output_path.to_string_lossy()
    );
    write_mim_index(&output_path, &index)?;

    info!(
        "zran: wrote index with {} access points to {}",
        index.checkpoints.len(),
        output_path.to_string_lossy()
    );

    Ok(())
}

/// Write index to a gzipped file.
fn write_mim_index(path: &Path, index: &MimIndex) -> Result<(), IndexError> {
    let writer = File::create(path)?;
    let mut writer = io::BufWriter::new(writer);

    // copy with the gzipped fields zeroed out.
    let copy = MimIndex {
        checkpoints: Vec::new(),
        metadata: index.metadata.clone(),
        ..*index
    };
    writer.write_all(MIMINDEX_FILE_CONSTANT)?;
    bincode::encode_into_std_write(copy, &mut writer, bincode::config::legacy())
        .map_err(|e| IndexError::Compression(format!("Failed to encode index: {}", e)))?;
    let mut gz_writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());
    bincode::encode_into_std_write(
        &index.checkpoints,
        &mut gz_writer,
        bincode::config::legacy(),
    )
    .map_err(|e| IndexError::Compression(format!("Failed to encode index: {}", e)))?;
    Ok(())
}
