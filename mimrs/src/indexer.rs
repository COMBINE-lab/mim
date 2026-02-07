use crate::mim_types::{
    Blake3Hash, DecompressionMode, DeflateCheckPoint, MimIndex, RecordCheckpoint,
    MIMINDEX_FILE_CONSTANT,
};
use blake3::Hasher;
//use libz_ng_sys::z_stream;
//use libz_ng_sys::{self as zlib, Z_OK};

use libz_rs_sys::{self as zlib};
use needletail::parse_fastx_file;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::Path;
use std::ptr;
use tracing::{info, trace};

// Constants
const WINSIZE: usize = 32768; // sliding window size
const CHUNK: usize = 16384; // file input buffer size

// Wrapper around Read that allows peeking a single byte
struct PeekableReader<R: Read> {
    inner: R,
    buffer: [u8; 1],
    is_buffered: bool,
}

impl<R: Read> PeekableReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: [0_u8],
            is_buffered: false,
        }
    }

    fn peek_byte(&mut self) -> io::Result<Option<u8>> {
        if !self.is_buffered {
            match self.inner.read(&mut self.buffer)? {
                0 => Ok(None),
                _ => {
                    self.is_buffered = true;
                    Ok(Some(self.buffer[0]))
                }
            }
        } else {
            Ok(Some(self.buffer[0]))
        }
    }
}

impl<R: Read> Read for PeekableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut total = 0;

        // First use the buffered byte if we have it
        if self.is_buffered {
            buf[total] = self.buffer[0];
            total += 1;
            self.is_buffered = false;
        }

        // Then read from inner if we need more
        if total < buf.len() {
            total += self.inner.read(&mut buf[total..])?;
        }

        Ok(total)
    }
}

/// Metadata structure
#[derive(Debug, Serialize, Deserialize)]
struct IndexMetadata {
    version: String,
    user_metadata: Option<JsonValue>,
}

impl MimIndex {
    fn new() -> Self {
        Self {
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
    in_offset: i64,
    out: i64,
    beg: i64,
    window: &[u8; WINSIZE],
    strm: &zlib::z_stream,
    chunk_size: i64,
) -> Result<(), IndexError> {
    // Calculate window dictionary size
    let dict_size = if out - beg > WINSIZE as i64 {
        WINSIZE
    } else {
        (out - beg) as usize
    };

    let bits = (strm.data_type & 7) as u8;
    let mut dict = vec![0u8; dict_size];

    // Copy the sliding window data
    // avail_out tells us how much of the window hasn't been filled yet
    let recent = WINSIZE - strm.avail_out as usize;
    let copy = recent.min(dict_size);

    // Copy the most recent data
    dict[dict_size - copy..].copy_from_slice(&window[recent - copy..recent]);

    // If we need more, wrap around to the beginning of the window
    let remaining = dict_size - copy;
    if remaining > 0 {
        dict[..remaining].copy_from_slice(&window[WINSIZE - remaining..WINSIZE]);
    }

    index.checkpoints.push(DeflateCheckPoint {
        plain_offset: out,
        gz_offset: in_offset,
        bits: bits.into(),
        dictionary_size: dict_size as u32,
        window: dict,
    });

    index.num_checkpoints += 1;

    if index.num_checkpoints % 10 == 0 {
        trace!(
            "adding access point {} at {} (read) {} (written); distance {} vs chunk size {}",
            index.num_checkpoints,
            in_offset,
            out,
            out - (if index.num_checkpoints > 1 {
                index.checkpoints[index.num_checkpoints as usize - 2].plain_offset
            } else {
                0
            }),
            chunk_size
        );
    }

    Ok(())
}

struct DecompressionState {
    strm: zlib::z_stream,
    window: [u8; WINSIZE],
    buf: [u8; CHUNK],
    totin: i64,
    totout: i64,
    mode: DecompressionMode,
    last: i64,
    beg: i64,
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
            window: [0u8; WINSIZE],
            buf: [0u8; CHUNK],
            totin: 0,
            totout: 0,
            mode: DecompressionMode::NONE,
            last: 0,
            beg: 0,
        }
    }

    fn init_inflate(&mut self, mode: DecompressionMode) -> i32 {
        unsafe {
            zlib::inflateInit2_(
                &mut self.strm,
                mode as i32,
                zlib::zlibVersion(),
                std::mem::size_of::<zlib::z_stream>() as i32,
            )
        }
    }

    fn inflate(&mut self, flush: i32) -> i32 {
        unsafe { zlib::inflate(&mut self.strm, flush) }
    }

    fn reset_inflate(&mut self) -> i32 {
        unsafe { zlib::inflateReset2(&mut self.strm, DecompressionMode::GZIP as i32) }
    }

    fn end_inflate(&mut self) {
        unsafe {
            zlib::inflateEnd(&mut self.strm);
        }
    }
}

/// Handle the boundary between concatenated gzip members
fn handle_gzip_member_boundary<R: Read>(
    reader: &mut PeekableReader<R>,
    state: &mut DecompressionState,
    ret: &mut i32,
) -> Result<(), IndexError> {
    if *ret == zlib::Z_STREAM_END && state.mode == DecompressionMode::GZIP {
        // Check if there's more data: either in buffer or by peeking into file
        let has_avail = state.strm.avail_in > 0;
        let has_peek = if !has_avail {
            reader.peek_byte()?.is_some()
        } else {
            false
        };
        let has_more_data = has_avail || has_peek;

        trace!(
            "Z_STREAM_END detected: avail_in={}, peek={}, has_more={}, beg={}, totout={}",
            state.strm.avail_in,
            has_peek,
            has_more_data,
            state.beg,
            state.totout
        );

        if has_more_data {
            // There is more input after the end of a gzip member
            // Reset the inflate state to read another gzip member
            // On success, this sets ret back to Z_OK to continue decompressing
            *ret = state.reset_inflate();
            trace!("Called inflateReset2, ret={}", ret);

            if *ret == zlib::Z_OK {
                state.beg = state.totout; // Reset history
                trace!("Reset beg to {}", state.beg);
            }
        }
    }
    Ok(())
}

/// Build a deflate index from a gzip file using raw zlib API
fn deflate_index_build<R: Read>(
    reader: &mut PeekableReader<R>,
    chunk_size: i64,
) -> Result<MimIndex, IndexError> {
    let mut hasher = Hasher::new();
    let mut index = MimIndex::new();
    let mut state = DecompressionState::new();

    let mut ret: i32 = zlib::Z_OK;

    // Main decompression loop
    'main_loop: loop {
        // Assure available input
        if state.strm.avail_in == 0 {
            let bytes_read = reader.read(&mut state.buf)?;

            if bytes_read > 0 {
                if bytes_read >= 2 && state.buf[0] == 0x1f && state.buf[1] == 0x8b {
                    trace!(
                        "Read new gzip header at totin={}, totout={}, checkpoint={}",
                        state.totin,
                        state.totout,
                        index.num_checkpoints
                    );
                }

                hasher.update(&state.buf[..bytes_read]);
                state.totin += bytes_read as i64;
            }

            state.strm.avail_in = bytes_read as u32;
            state.strm.next_in = state.buf.as_mut_ptr();

            if state.mode == DecompressionMode::NONE && bytes_read > 0 {
                state.mode = if state.buf[0] & 0x0f == 8 {
                    DecompressionMode::ZLIB
                } else if state.buf[0] == 0x1f {
                    DecompressionMode::GZIP
                } else {
                    DecompressionMode::RAW
                };

                ret = state.init_inflate(state.mode);
                if ret != zlib::Z_OK {
                    break 'main_loop;
                }
            }
        }

        // Assure available output
        if state.strm.avail_out == 0 {
            state.strm.avail_out = WINSIZE as u32;
            state.strm.next_out = state.window.as_mut_ptr();
        }

        // Handle special case for RAW mode at the start
        if state.mode == DecompressionMode::RAW && index.num_checkpoints == 0 {
            state.strm.data_type = 0x80;
        } else {
            let before = state.strm.avail_out as i64;
            ret = state.inflate(zlib::Z_BLOCK);
            let after = state.strm.avail_out as i64;
            let produced = before - after;
            state.totout += produced;
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
        }

        // Check if we should add an access point
        if (state.strm.data_type & 0xc0) == 0x80
            && (index.num_checkpoints == 0 || state.totout - state.last >= chunk_size)
        {
            let in_offset = state.totin - state.strm.avail_in as i64;
            add_point(
                &mut index,
                in_offset,
                state.totout,
                state.beg,
                &state.window,
                &state.strm,
                chunk_size,
            )?;
            state.last = state.totout;
        }

        // Handle end of gzip member
        handle_gzip_member_boundary(reader, &mut state, &mut ret)?;

        if ret != zlib::Z_OK {
            break 'main_loop;
        }
    }

    state.end_inflate();

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
    index.plain_size = state.totout;

    Ok(index)
}

/// Build index from a gzip file with FASTQ record tracking
pub fn build_index(
    gzip_file: &Path,
    chunk_size: i64,
    user_metadata: Option<JsonValue>,
    output_file: Option<&Path>,
) -> Result<(), IndexError> {
    trace!("Opening file: {:?}", gzip_file);
    let file = File::open(gzip_file)?;
    let buf_reader = BufReader::with_capacity(1024 * 1024, file);
    let mut peekable_reader = PeekableReader::new(buf_reader);

    trace!("Building deflate index...");
    let mut index = deflate_index_build(&mut peekable_reader, chunk_size)?;

    info!(
        "zran: built index with {} access points!",
        index.num_checkpoints
    );

    // Create metadata
    let metadata = IndexMetadata {
        version: "1.0.0".to_string(),
        user_metadata,
    };

    index.metadata =
        serde_cbor::to_vec(&metadata).map_err(|e| IndexError::Compression(e.to_string()))?;

    // Process FASTQ records
    trace!("Getting record boundaries from FASTQ file");

    let mut record_count = 0u64;

    if index.num_checkpoints == 0 {
        info!("no access points created");
        index.record_boundaries.push(RecordCheckpoint {
            first_record_in_chunk: 0,
            byte_offset: 0,
        });

        // Parse FASTQ file to count records
        let mut reader = parse_fastx_file(gzip_file)
            .map_err(|e| IndexError::Compression(format!("Failed to parse FASTQ: {}", e)))?;

        while let Some(record) = reader.next() {
            record.map_err(|e| IndexError::Compression(format!("FASTQ parse error: {}", e)))?;
            record_count += 1;
        }

        index.record_boundaries.push(RecordCheckpoint {
            first_record_in_chunk: record_count,
            byte_offset: index.plain_size as u64,
        });
        index.num_record_chunks = 1;
    } else {
        let mut current_access_index = 0;
        let mut next_decomp_checkpoint = index.checkpoints[current_access_index].plain_offset;

        // Parse FASTQ and align with access points
        let mut reader = parse_fastx_file(gzip_file)
            .map_err(|e| IndexError::Compression(format!("Failed to parse FASTQ: {}", e)))?;

        while let Some(record_result) = reader.next() {
            let record = record_result
                .map_err(|e| IndexError::Compression(format!("FASTQ parse error: {}", e)))?;

            // Get the byte offset where this record starts in the uncompressed stream
            let record_start = record.position().byte() as i64; //reader.position().byte() as i64;

            // Check if we've passed a checkpoint
            if record_start >= next_decomp_checkpoint
                && current_access_index < index.num_checkpoints as usize
            {
                if index.record_boundaries.len() % 100 == 0 {
                    trace!(
                        "matched checkpoint {} with record starting at {} (record num {})",
                        next_decomp_checkpoint,
                        record_start,
                        record_count
                    );
                }

                index.record_boundaries.push(RecordCheckpoint {
                    first_record_in_chunk: record_count,
                    byte_offset: record_start as u64,
                });

                current_access_index += 1;
                if current_access_index < index.num_checkpoints as usize {
                    next_decomp_checkpoint = index.checkpoints[current_access_index].plain_offset;
                }
            }

            record_count += 1;
        }

        index.record_boundaries.push(RecordCheckpoint {
            first_record_in_chunk: record_count,
            byte_offset: index.plain_size as u64,
        });
        index.num_record_chunks = index.record_boundaries.len() as i64;
    }

    index.total_num_records = record_count as i64;
    info!("Got {} records from FASTQ file.", record_count);

    // Save index
    let output_path = output_file
        .map(|s| s.to_owned())
        .unwrap_or_else(|| {
            let pb = std::path::PathBuf::from(gzip_file);
            pb.with_additional_extension(".mim")
        })
        .clone();

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
