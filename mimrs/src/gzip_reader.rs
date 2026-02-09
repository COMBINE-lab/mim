use crate::mim_types::{DecompressionMode, MimIndex};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use tracing::{debug, trace};

/// Buffer size for the input file.
/// Around L2 cache size and a bit smaller than the 1MB of the indexer,
/// so that this doesn't overload L3 when run on many threads.
const INPUT_BUF_SIZE: usize = 256 * 1024;

/// Wrapper around zlib's z_stream implementing Drop and encapsulating the unsafe.
pub(crate) struct ZStreamWrapper {
    pub strm: libz_rs_sys::z_stream,
    initialized: bool,
}

impl ZStreamWrapper {
    pub fn new() -> Self {
        let mut strm: libz_rs_sys::z_stream = unsafe { std::mem::zeroed() };
        strm.zalloc = None;
        strm.zfree = None;

        ZStreamWrapper {
            strm,
            initialized: false,
        }
    }

    pub fn init(&mut self, mode: DecompressionMode) -> io::Result<()> {
        assert!(!self.initialized);

        let ret = unsafe {
            libz_rs_sys::inflateInit2_(
                &mut self.strm,
                mode as i32,
                libz_rs_sys::zlibVersion(),
                std::mem::size_of::<libz_rs_sys::z_stream>() as i32,
            )
        };

        if ret != libz_rs_sys::Z_OK {
            return Err(io::Error::other(format!("inflateInit2 failed: {}", ret)));
        }

        self.initialized = true;
        Ok(())
    }

    /// Set the LZ context window.
    ///
    /// NOTE: Even though this calls `SetDictionary`, the Huffman dictionary is actually encoded
    /// _inside_ each DEFLATE block, and this "dictionary" rather refers to the 32kiB preceding context.
    pub fn set_context(&mut self, dict: &[u8]) -> io::Result<()> {
        let ret = unsafe {
            libz_rs_sys::inflateSetDictionary(&mut self.strm, dict.as_ptr(), dict.len() as u32)
        };

        if ret != libz_rs_sys::Z_OK {
            return Err(io::Error::other(format!(
                "inflateSetDictionary failed: {}",
                ret
            )));
        }

        Ok(())
    }

    /// Push bits to the front of the stream.
    pub fn insert_bits(&mut self, bits: i32, value: i32) -> io::Result<()> {
        let ret = unsafe { libz_rs_sys::inflatePrime(&mut self.strm, bits, value) };

        if ret != libz_rs_sys::Z_OK {
            return Err(io::Error::other(format!("inflatePrime failed: {}", ret)));
        }

        Ok(())
    }

    /// Reset the stream state.
    pub fn reset(&mut self, mode: DecompressionMode) -> io::Result<()> {
        let ret = unsafe { libz_rs_sys::inflateReset2(&mut self.strm, mode as i32) };
        if ret != libz_rs_sys::Z_OK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("inflate error: {}", ret),
            ));
        }
        Ok(())
    }

    /// Inflate some data.
    ///
    /// Returns `(end_of_stream, consumed, produced)` bytes.
    pub fn inflate(&mut self, mode: i32) -> io::Result<(bool, usize, usize)> {
        let in_before = self.strm.avail_in as i64;
        let out_before = self.strm.avail_out as i64;
        let ret = unsafe { libz_rs_sys::inflate(&mut self.strm, mode) };
        let in_after = self.strm.avail_in as i64;
        let out_after = self.strm.avail_out as i64;
        let consumed = in_before - in_after;
        let produced = out_before - out_after;

        if ret != libz_rs_sys::Z_OK && ret != libz_rs_sys::Z_STREAM_END {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("inflate error: {}", ret),
            ));
        }
        Ok((
            ret == libz_rs_sys::Z_STREAM_END,
            consumed as usize,
            produced as usize,
        ))
    }

    /// Free
    pub fn end(&mut self) -> io::Result<()> {
        if self.initialized {
            let ret = unsafe { libz_rs_sys::inflateEnd(&mut self.strm) };
            if ret != libz_rs_sys::Z_OK {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("inflateEnd failed: {}", ret),
                ));
            }

            self.initialized = false;
        }
        Ok(())
    }
}

unsafe impl Send for ZStreamWrapper {}
impl Drop for ZStreamWrapper {
    fn drop(&mut self) {
        let _ = self.end();
    }
}

/// Reader for streaming decompression from a gzip checkpoint
pub struct GzipStreamReader {
    file: BufReader<File>,
    zstream: ZStreamWrapper,
    out_pos: u64,
    file_mode: DecompressionMode,
    current_mode: DecompressionMode,
}

impl GzipStreamReader {
    /// Open a gzip file at a specific checkpoint
    pub fn open_at_checkpoint(
        gz_file_path: &Path,
        index: &MimIndex,
        checkpoint_index: usize,
    ) -> io::Result<Self> {
        if checkpoint_index >= index.checkpoints.len() as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid checkpoint index",
            ));
        }

        let file = File::open(gz_file_path)?;
        let mut file = BufReader::with_capacity(INPUT_BUF_SIZE, file);

        // Get the checkpoint
        let checkpoint = &index.checkpoints[checkpoint_index];

        // Seek to the compressed position
        let seek_pos = if checkpoint.bits > 0 {
            checkpoint.in_pos - 1
        } else {
            checkpoint.in_pos
        };

        // NOTE: We start directly at RAW DEFLATE blocks, and skip any gzip headers.
        file.seek(SeekFrom::Start(seek_pos as u64))?;
        let mut zstream = ZStreamWrapper::new();
        zstream.init(DecompressionMode::RAW)?;

        // Set the decompression dictionary FIRST (before any inflation)
        if checkpoint.out_pos > 0 {
            zstream.set_context(&checkpoint.window)?;
        }

        // Handle bit-level alignment
        if checkpoint.bits > 0 {
            // We're starting mid-byte. Read that byte and prime the bit buffer.
            let mut last_byte = [0u8; 1];
            file.read_exact(&mut last_byte)?;

            // Use inflatePrime exactly as the C++ code does:
            let bit_value = (last_byte[0] >> (8 - checkpoint.bits)) as i32;
            zstream.insert_bits(checkpoint.bits as i32, bit_value)?;
        }

        Ok(GzipStreamReader {
            file,
            zstream,
            out_pos: checkpoint.out_pos as u64,
            file_mode: index.mode,
            current_mode: DecompressionMode::RAW,
        })
    }
    /// Get current uncompressed offset
    pub fn out_pos(&self) -> u64 {
        self.out_pos
    }
}

impl io::Read for GzipStreamReader {
    /// Read decompressed data from the stream
    /// Handles multi-member gzip files (including BGZF)
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let len = buffer.len();
        // FIXME: Handle the chunk-end: down-size buffer as needed.
        let mut total_copied = 0;

        self.zstream.strm.next_out = buffer.as_mut_ptr();
        self.zstream.strm.avail_out = len as u32;

        while total_copied < len {
            let strm = &mut self.zstream.strm;

            let input_buffer = self.file.fill_buf()?;
            if input_buffer.is_empty() {
                return Ok(total_copied);
            }
            strm.avail_in = input_buffer.len() as u32;
            strm.next_in = input_buffer.as_ptr();
            // WAS: Z_NO_FLUSH
            let (end_of_stream, consumed, produced) = self.zstream.inflate(libz_rs_sys::Z_BLOCK)?;
            self.file.consume(consumed as usize);

            // trace!(
            //     "STATE: {:>16b} ret {ret} consumed {}  produced {}",
            //     strm.data_type, consumed, produced
            // );

            total_copied += produced as usize;
            self.out_pos += produced as u64;

            if end_of_stream {
                trace!("Z_STREAM_END detected");

                // Skip 8-byte CRC suffix that ends the zlib/gzip block.
                // Only needed if we started in raw deflate mode -- otherwise inflate()
                // calls will already handle this.
                if self.current_mode == DecompressionMode::RAW
                    && self.file_mode != DecompressionMode::RAW
                {
                    // Read the 8-byte CRC.
                    self.file.read_exact(&mut [0u8; 8])?;
                    self.current_mode = self.file_mode;
                }

                // Check if we're at the end.
                let input_buffer = self.file.fill_buf()?;
                if input_buffer.is_empty() {
                    return Ok(total_copied);
                }

                // If not at the end, reset libz.
                self.zstream.reset(self.current_mode)?;
            }
        }

        Ok(total_copied)
    }
}

// Example usage
pub fn example_usage(gz_file: &Path, index: &MimIndex, checkpoint_idx: usize) -> io::Result<()> {
    let mut reader = GzipStreamReader::open_at_checkpoint(gz_file, index, checkpoint_idx)?;

    const BUFSIZE: usize = 131072;

    let mut buffer = vec![0u8; BUFSIZE]; // 128KB
    let mut total_read = 0;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        total_read += bytes_read;
        // Process buffer[..bytes_read]
    }

    println!("Successfully read {} bytes from checkpoint", total_read);
    Ok(())
}
