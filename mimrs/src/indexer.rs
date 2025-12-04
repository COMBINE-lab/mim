use crate::mim_types::{BLAKE3_OUT_LEN, DeflateIndex, MIMINDEX_STR, Point, RecordCheckpoint};
use blake3::Hasher;
//use libz_ng_sys::z_stream;
//use libz_ng_sys::{self as zlib, Z_OK};

use libz_rs_sys::{self as zlib};
use needletail::parse_fastx_file;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::Path;
use std::ptr;
use tracing::{info, trace};

// Constants
const WINSIZE: usize = 32768; // sliding window size
const CHUNK: usize = 16384; // file input buffer size

// Decompression modes
const RAW: i32 = -15;
const ZLIB: i32 = 15;
const GZIP: i32 = 31;

// Wrapper around Read that allows peeking
struct PeekableReader<R: Read> {
    inner: R,
    buffer: VecDeque<u8>,
}

impl<R: Read> PeekableReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: VecDeque::new(),
        }
    }

    fn peek_byte(&mut self) -> io::Result<Option<u8>> {
        if self.buffer.is_empty() {
            let mut byte = [0u8; 1];
            match self.inner.read(&mut byte)? {
                0 => Ok(None),
                _ => {
                    self.buffer.push_back(byte[0]);
                    Ok(Some(byte[0]))
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

        // First drain any buffered bytes
        while total < buf.len() && !self.buffer.is_empty() {
            buf[total] = self.buffer.pop_front().unwrap();
            total += 1;
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

impl DeflateIndex {
    fn new() -> Self {
        Self {
            metadata_dict: Vec::new(),
            have: 0,
            mode: 0,
            length: 0,
            list: Vec::new(),
            record_boundaries: Vec::new(),
            num_record_chunks: 0,
            total_record_count: 0,
            compressed_hash: [0u8; BLAKE3_OUT_LEN],
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
    index: &mut DeflateIndex,
    in_offset: i64,
    out: i64,
    beg: i64,
    window: &[u8; WINSIZE],
    strm: &zlib::z_stream,
    span: i64,
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

    index.list.push(Point {
        out,
        in_offset,
        bits: bits.into(),
        dict: dict_size as u32,
        window: dict,
    });

    index.have += 1;

    if index.have % 10 == 0 {
        trace!(
            "adding access point {} at {} (read) {} (written); distance {} vs span {}",
            index.have,
            in_offset,
            out,
            out - (if index.have > 1 {
                index.list[index.have as usize - 2].out
            } else {
                0
            }),
            span
        );
    }

    Ok(())
}

/// Build a deflate index from a gzip file using raw zlib API
fn deflate_index_build<R: Read>(
    reader: &mut PeekableReader<R>,
    span: i64,
) -> Result<DeflateIndex, IndexError> {
    let mut hasher = Hasher::new();
    let mut index = DeflateIndex::new();

    let mut buf = [0u8; CHUNK];
    let mut window = [0u8; WINSIZE];
    let mut totin: i64 = 0;
    let mut totout: i64 = 0;
    let mut beg: i64 = 0;
    let mut mode: i32 = 0;
    let mut last: i64 = 0;

    // Initialize z_stream
    let mut strm: zlib::z_stream = unsafe { std::mem::zeroed() };

    strm.zalloc = None;
    strm.zfree = None;
    strm.opaque = ptr::null_mut();
    strm.avail_in = 0;
    strm.next_in = ptr::null_mut();

    let mut ret: i32 = zlib::Z_OK;

    // Main decompression loop - matches C++ do-while (ret == Z_OK)
    'main_loop: loop {
        // Assure available input - only read when we've consumed all previous input
        if strm.avail_in == 0 {
            let bytes_read = reader.read(&mut buf)?;

            if bytes_read > 0 {
                // Check if this looks like a new gzip member header
                if bytes_read >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
                    trace!(
                        "Read new gzip header at totin={}, totout={}, checkpoint={}",
                        totin, totout, index.have
                    );
                }

                // Hash what we just read - this is what will be decompressed
                hasher.update(&buf[..bytes_read]);
                totin += bytes_read as i64;
            }

            strm.avail_in = bytes_read as u32;
            strm.next_in = buf.as_mut_ptr();

            if mode == 0 && bytes_read > 0 {
                // Determine the compression mode
                mode = if buf[0] & 0x0f == 8 {
                    ZLIB
                } else if buf[0] == 0x1f {
                    GZIP
                } else {
                    RAW
                };

                // Initialize inflation
                ret = unsafe {
                    zlib::inflateInit2_(
                        &mut strm,
                        mode,
                        zlib::zlibVersion(),
                        std::mem::size_of::<zlib::z_stream>() as i32,
                    )
                };
                if ret != zlib::Z_OK {
                    break 'main_loop;
                }
            }
        }

        // Assure available output
        if strm.avail_out == 0 {
            strm.avail_out = WINSIZE as u32;
            strm.next_out = window.as_mut_ptr();
        }

        // Handle special case for RAW mode at the start
        if mode == RAW && index.have == 0 {
            // For raw deflate, set data_type to simulate end of header
            strm.data_type = 0x80;
        } else {
            // Inflate and update uncompressed bytes
            let before = strm.avail_out as i64;
            ret = unsafe { zlib::inflate(&mut strm, zlib::Z_BLOCK) };
            let after = strm.avail_out as i64;
            let produced = before - after;
            totout += produced;

            // NOTE: Tracking: https://github.com/trifectatechfoundation/zlib-rs/issues/439
            // Handle Z_DATA_ERROR that occurs at gzip member boundaries
            // When using Z_BLOCK mode with concatenated gzip files, inflate() can return
            // Z_DATA_ERROR when it encounters the length check at the end of a member,
            // especially if the member boundary doesn't align with a block boundary.
            // If we produced no output and we're in GZIP mode, treat this as Z_STREAM_END
            // to allow processing of the next member.
            if ret == zlib::Z_DATA_ERROR && mode == GZIP && produced == 0 {
                // This is likely a member boundary issue, treat as end of stream
                ret = zlib::Z_STREAM_END;
            }
        }

        // Check if we should add an access point
        if (strm.data_type & 0xc0) == 0x80 && (index.have == 0 || totout - last >= span) {
            // We're at the end of a header or a non-last deflate block
            let in_offset = totin - strm.avail_in as i64;
            add_point(&mut index, in_offset, totout, beg, &window, &strm, span)?;
            last = totout;
        }

        // Handle end of gzip member - check for concatenated gzip streams
        // IMPORTANT: This happens BEFORE we check the loop condition
        if ret == zlib::Z_STREAM_END && mode == GZIP {
            // Check if there's more data: either in buffer or by peeking into file
            let has_avail = strm.avail_in > 0;
            let has_peek = if !has_avail {
                reader.peek_byte()?.is_some()
            } else {
                false
            };
            let has_more_data = has_avail || has_peek;

            trace!(
                "Z_STREAM_END detected: avail_in={}, peek={}, has_more={}, beg={}, totout={}",
                strm.avail_in, has_peek, has_more_data, beg, totout
            );

            if has_more_data {
                // There is more input after the end of a gzip member
                // Reset the inflate state to read another gzip member
                // On success, this sets ret back to Z_OK to continue decompressing
                ret = unsafe { zlib::inflateReset2(&mut strm, GZIP) };
                trace!("Called inflateReset2, ret={}", ret);
                if ret != zlib::Z_OK {
                    break 'main_loop;
                }
                beg = totout; // Reset history - CRITICAL!
                trace!("Reset beg to {}", beg);
            }
        }

        // Keep going until Z_STREAM_END or error
        // The C++ code's loop is: do { ... } while (ret == Z_OK);
        if ret != zlib::Z_OK {
            break 'main_loop;
        }
    }

    unsafe {
        zlib::inflateEnd(&mut strm);
    }

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
    index.compressed_hash.copy_from_slice(hash.as_bytes());

    let mut msg = vec![0_u8; 128];
    write!(&mut msg, "BLAKE3 checksum:")?;
    for byte in hash.as_bytes() {
        write!(&mut msg, "{:02x}", byte)?;
    }
    info!("{}", str::from_utf8(&msg).expect("valid utf8"));

    index.mode = mode;
    index.length = totout;

    Ok(index)
}

/*
/// Build a deflate index from a gzip file using raw zlib API
fn deflate_index_build_zlibng<R: Read>(
    reader: &mut PeekableReader<R>,
    span: i64,
) -> Result<DeflateIndex, IndexError> {
    let mut hasher = Hasher::new();
    let mut index = DeflateIndex::new();

    let mut buf = [0u8; CHUNK];
    let mut window = [0u8; WINSIZE];
    let mut totin: i64 = 0;
    let mut totout: i64 = 0;
    let mut beg: i64 = 0;
    let mut mode: i32 = 0;
    let mut last: i64 = 0;

    // Initialize z_stream
    // let mut strm: zlib::z_stream = unsafe { std::mem::zeroed() };
    let layout = std::alloc::Layout::new::<z_stream>();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) as *mut z_stream };
    let mut strm = unsafe { &mut (*ptr) };

    //strm.zalloc = None;
    //strm.zfree = None;
    strm.opaque = ptr::null_mut();
    strm.avail_in = 0;
    strm.next_in = ptr::null_mut();

    let mut ret: i32 = zlib::Z_OK;

    // Main decompression loop - matches C++ do-while (ret == Z_OK)
    'main_loop: loop {
        // Assure available input - only read when we've consumed all previous input
        if strm.avail_in == 0 {
            let bytes_read = reader.read(&mut buf)?;

            if bytes_read > 0 {
                // Check if this looks like a new gzip member header
                if bytes_read >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
                    eprintln!(
                        "Read new gzip header at totin={}, totout={}, checkpoint={}",
                        totin, totout, index.have
                    );
                }

                // Hash what we just read - this is what will be decompressed
                hasher.update(&buf[..bytes_read]);
                totin += bytes_read as i64;
            }

            strm.avail_in = bytes_read as u32;
            strm.next_in = buf.as_mut_ptr();

            if mode == 0 && bytes_read > 0 {
                // Determine the compression mode
                mode = if buf[0] & 0x0f == 8 {
                    ZLIB
                } else if buf[0] == 0x1f {
                    GZIP
                } else {
                    RAW
                };

                // Initialize inflation
                ret = unsafe {
                    zlib::inflateInit2_(
                        &mut *strm,
                        mode,
                        zlib::zlibVersion(),
                        std::mem::size_of::<zlib::z_stream>() as i32,
                    )
                };
                if ret != zlib::Z_OK {
                    break 'main_loop;
                }
            }
        }

        // Assure available output
        if strm.avail_out == 0 {
            strm.avail_out = WINSIZE as u32;
            strm.next_out = window.as_mut_ptr();
        }

        // Handle special case for RAW mode at the start
        if mode == RAW && index.have == 0 {
            // For raw deflate, set data_type to simulate end of header
            strm.data_type = 0x80;
        } else {
            // Inflate and update uncompressed bytes
            let before = strm.avail_out as i64;
            ret = unsafe { zlib::inflate(&mut *strm, zlib::Z_BLOCK) };
            let after = strm.avail_out as i64;
            let produced = before - after;
            totout += produced;

            // Special handling for Z_DATA_ERROR with "incorrect length check"
            // This can happen at gzip member boundaries in concatenated gzip files
            if ret == zlib::Z_DATA_ERROR && mode == GZIP && produced == 0 {
                // Check if this is a length check error at end of member
                let msg = unsafe {
                    if !strm.msg.is_null() {
                        std::ffi::CStr::from_ptr(strm.msg).to_str().unwrap_or("")
                    } else {
                        ""
                    }
                };

                if msg.contains("incorrect length") {
                    eprintln!(
                        "WARNING: Got 'incorrect length check' at member boundary, treating as Z_STREAM_END"
                    );
                    eprintln!(
                        "  at totin={}, totout={}, avail_in={}",
                        totin, totout, strm.avail_in
                    );
                    // Treat this as end of stream and try to recover
                    ret = zlib::Z_STREAM_END;
                }
            }
        }

        // Check if we should add an access point
        if (strm.data_type & 0xc0) == 0x80 && (index.have == 0 || totout - last >= span) {
            // We're at the end of a header or a non-last deflate block
            let in_offset = totin - strm.avail_in as i64;
            add_point(&mut index, in_offset, totout, beg, &window, &strm, span)?;
            last = totout;
        }

        // Handle end of gzip member - check for concatenated gzip streams
        // IMPORTANT: This happens BEFORE we check the loop condition
        if ret == zlib::Z_STREAM_END && mode == GZIP {
            // Check if there's more data: either in buffer or by peeking into file
            let has_avail = strm.avail_in > 0;
            let has_peek = if !has_avail {
                reader.peek_byte()?.is_some()
            } else {
                false
            };
            let has_more_data = has_avail || has_peek;

            eprintln!(
                "Z_STREAM_END detected: avail_in={}, peek={}, has_more={}, beg={}, totout={}",
                strm.avail_in, has_peek, has_more_data, beg, totout
            );

            if has_more_data {
                // There is more input after the end of a gzip member
                // Reset the inflate state to read another gzip member
                // On success, this sets ret back to Z_OK to continue decompressing
                ret = unsafe { zlib::inflateReset2(&mut *strm, GZIP) };
                eprintln!("Called inflateReset2, ret={}", ret);
                if ret != zlib::Z_OK {
                    break 'main_loop;
                }
                beg = totout; // Reset history - CRITICAL!
                eprintln!("Reset beg to {}", beg);
            }
        }

        // Keep going until Z_STREAM_END or error
        // The C++ code's loop is: do { ... } while (ret == Z_OK);
        if ret != zlib::Z_OK {
            break 'main_loop;
        }
    }

    unsafe {
        zlib::inflateEnd(&mut *strm);
    }

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
    index.compressed_hash.copy_from_slice(hash.as_bytes());

    println!("BLAKE3 checksum:");
    for byte in hash.as_bytes() {
        print!("{:02x}", byte);
    }
    println!();

    index.mode = mode;
    index.length = totout;

    Ok(index)
}
*/

/// Build index from a gzip file with FASTQ record tracking
pub fn build_index<P: AsRef<Path>>(
    gzip_file: P,
    span: i64,
    user_metadata: Option<JsonValue>,
    output_file: Option<P>,
) -> Result<(), IndexError> {
    trace!("Opening file: {:?}", gzip_file.as_ref());
    let file = File::open(gzip_file.as_ref())?;
    let buf_reader = BufReader::with_capacity(1024 * 1024, file);
    let mut peekable_reader = PeekableReader::new(buf_reader);

    trace!("Building deflate index...");
    let mut index = deflate_index_build(&mut peekable_reader, span)?;

    info!("zran: built index with {} access points!", index.have);

    // Create metadata
    let metadata = IndexMetadata {
        version: "1.0.0".to_string(),
        user_metadata,
    };

    index.metadata_dict =
        serde_cbor::to_vec(&metadata).map_err(|e| IndexError::Compression(e.to_string()))?;

    // Process FASTQ records
    trace!("Getting record boundaries from FASTQ file");

    let mut record_count = 0u64;

    if index.have == 0 {
        info!("no access points created");
        index.record_boundaries.push(RecordCheckpoint {
            first_record_in_chunk: 0,
            byte_offset: 0,
        });

        // Parse FASTQ file to count records
        let mut reader = parse_fastx_file(gzip_file.as_ref())
            .map_err(|e| IndexError::Compression(format!("Failed to parse FASTQ: {}", e)))?;

        while let Some(record) = reader.next() {
            record.map_err(|e| IndexError::Compression(format!("FASTQ parse error: {}", e)))?;
            record_count += 1;
        }

        index.record_boundaries.push(RecordCheckpoint {
            first_record_in_chunk: record_count,
            byte_offset: index.length as u64,
        });
        index.num_record_chunks = 1;
    } else {
        let mut current_access_index = 0;
        let mut next_decomp_checkpoint = index.list[current_access_index].out;

        // Parse FASTQ and align with access points
        let mut reader = parse_fastx_file(gzip_file.as_ref())
            .map_err(|e| IndexError::Compression(format!("Failed to parse FASTQ: {}", e)))?;

        while let Some(record_result) = reader.next() {
            let record = record_result
                .map_err(|e| IndexError::Compression(format!("FASTQ parse error: {}", e)))?;

            // Get the byte offset where this record starts in the uncompressed stream
            let record_start = record.position().byte() as i64; //reader.position().byte() as i64;

            // Check if we've passed a checkpoint
            if record_start >= next_decomp_checkpoint && current_access_index < index.have as usize
            {
                if index.record_boundaries.len() % 100 == 0 {
                    trace!(
                        "matched checkpoint {} with record starting at {} (record num {})",
                        next_decomp_checkpoint, record_start, record_count
                    );
                }

                index.record_boundaries.push(RecordCheckpoint {
                    first_record_in_chunk: record_count,
                    byte_offset: record_start as u64,
                });

                current_access_index += 1;
                if current_access_index < index.have as usize {
                    next_decomp_checkpoint = index.list[current_access_index].out;
                }
            }

            record_count += 1;
        }

        index.record_boundaries.push(RecordCheckpoint {
            first_record_in_chunk: record_count,
            byte_offset: index.length as u64,
        });
        index.num_record_chunks = index.record_boundaries.len() as i64;
    }

    index.total_record_count = record_count as i64;
    info!("Got {} records from FASTQ file.", record_count);

    // Save index
    let output_path = output_file
        .map(|s| s.as_ref().to_owned())
        .unwrap_or_else(|| {
            let pb = std::path::PathBuf::from(gzip_file.as_ref());
            pb.with_extension("mim");
            pb
        })
        .clone();

    trace!(
        "zran: attempting to write index to {}",
        output_path.to_string_lossy()
    );
    save_index(&output_path, &index)?;

    info!(
        "zran: wrote index with {} access points to {}",
        index.have,
        output_path.to_string_lossy()
    );

    Ok(())
}

/// Save index to a gzipped file
fn save_index(path: &Path, index: &DeflateIndex) -> Result<(), IndexError> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let file = File::create(path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());

    // Write magic header
    encoder.write_all(MIMINDEX_STR.as_bytes())?;

    // Write metadata dictionary
    encoder.write_all(&(index.metadata_dict.len() as u64).to_le_bytes())?;
    encoder.write_all(&index.metadata_dict)?;

    // Write index metadata
    encoder.write_all(&index.mode.to_le_bytes())?;
    encoder.write_all(&index.have.to_le_bytes())?;
    encoder.write_all(&index.length.to_le_bytes())?;
    encoder.write_all(&index.num_record_chunks.to_le_bytes())?;
    encoder.write_all(&index.compressed_hash)?;

    // Write access points
    for point in &index.list {
        encoder.write_all(&point.out.to_le_bytes())?;
        encoder.write_all(&point.in_offset.to_le_bytes())?;
        encoder.write_all(&point.bits.to_le_bytes())?;
        encoder.write_all(&(point.dict).to_le_bytes())?;
        encoder.write_all(&point.window[..(point.dict as usize)])?;
    }

    // Write record boundaries
    let boundaries_count = index.record_boundaries.len() as u64;
    encoder.write_all(&boundaries_count.to_le_bytes())?;

    for boundary in &index.record_boundaries {
        encoder.write_all(&boundary.first_record_in_chunk.to_le_bytes())?;
        encoder.write_all(&boundary.byte_offset.to_le_bytes())?;
    }

    // Write total record count
    encoder.write_all(&index.total_record_count.to_le_bytes())?;

    encoder.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_creation() {
        let index = DeflateIndex::new();
        assert_eq!(index.have, 0);
        assert_eq!(index.mode, 0);
    }
}
