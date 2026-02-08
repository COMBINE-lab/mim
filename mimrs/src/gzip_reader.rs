use crate::indexer::detect_mode;
use crate::mim_types::{DecompressionMode, MimIndex};
use libz_rs_sys::{
    Z_BUF_ERROR, Z_OK, Z_STREAM_END, inflateEnd, inflatePrime, inflateReset, inflateSetDictionary,
    z_stream,
};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::ptr;
use tracing::trace;

const BUFSIZE: usize = 131072;

/// Safe wrapper around zlib's z_stream
struct ZStreamWrapper {
    stream: *mut z_stream,
    initialized: bool,
}

impl ZStreamWrapper {
    fn new() -> Self {
        // Allocate memory for z_stream on the heap
        let layout = std::alloc::Layout::new::<z_stream>();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) as *mut z_stream };

        if ptr.is_null() {
            panic!("Failed to allocate memory for z_stream");
        }

        ZStreamWrapper {
            stream: ptr,
            initialized: false,
        }
    }

    fn init(&mut self, mode: DecompressionMode) -> io::Result<()> {
        if self.initialized {
            unsafe { inflateEnd(self.stream) };
        }

        // Zero out the memory
        unsafe {
            ptr::write_bytes(self.stream, 0, 1);
        }

        let ret = unsafe {
            libz_rs_sys::inflateInit2_(
                self.stream,
                mode as i32,
                libz_rs_sys::zlibVersion(),
                std::mem::size_of::<z_stream>() as i32,
            )
        };

        if ret != Z_OK {
            return Err(io::Error::other(format!("inflateInit2 failed: {}", ret)));
        }

        let state_ptr = unsafe { (*self.stream).state };

        if state_ptr.is_null() {
            return Err(io::Error::other("inflateInit2 succeeded but state is null"));
        }

        self.initialized = true;
        Ok(())
    }

    fn set_dictionary(&mut self, dict: &[u8]) -> io::Result<()> {
        let ret = unsafe { inflateSetDictionary(self.stream, dict.as_ptr(), dict.len() as u32) };

        if ret != Z_OK {
            return Err(io::Error::other(format!(
                "inflateSetDictionary failed: {}",
                ret
            )));
        }

        Ok(())
    }

    fn prime(&mut self, bits: i32, value: i32) -> io::Result<()> {
        let ret = unsafe { inflatePrime(self.stream, bits, value) };

        if ret != Z_OK {
            return Err(io::Error::other(format!("inflatePrime failed: {}", ret)));
        }

        Ok(())
    }

    fn get_mut(&mut self) -> &mut z_stream {
        unsafe { &mut *self.stream }
    }
}

unsafe impl Send for ZStreamWrapper {}
impl Drop for ZStreamWrapper {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                inflateEnd(self.stream);
            }
        }
    }
}

/// Reader for streaming decompression from a gzip checkpoint
pub struct GzipStreamReader {
    file: BufReader<File>,
    zstream: ZStreamWrapper,
    uncompressed_offset: u64,
    mode: DecompressionMode,

    // Input buffer for compressed data
    input_buffer: Box<[u8; BUFSIZE]>, // 128KB

    file_offset: u64,
    // file_buffer: Option<Box<[u8]>>,
}

impl GzipStreamReader {
    /// Open a gzip file at a specific checkpoint
    pub fn open_at_checkpoint(
        gz_file_path: &Path,
        index: &MimIndex,
        checkpoint_index: usize,
    ) -> io::Result<Self> {
        if checkpoint_index >= index.num_checkpoints as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid checkpoint index",
            ));
        }

        let file = File::open(gz_file_path)?;
        let mut file = BufReader::new(file);

        // Get the checkpoint
        let checkpoint = &index.checkpoints[checkpoint_index];

        let uncompressed_offset = checkpoint.plain_pos as u64;
        // Seek to the compressed position
        let seek_pos = if checkpoint.bits > 0 {
            checkpoint.gz_pos - 1
        } else {
            checkpoint.gz_pos
        };

        // NOTE: We start directly at RAW DEFLATE blocks, and skip any gzip headers.
        file.seek(SeekFrom::Start(seek_pos as u64))?;
        let mut file_offset = seek_pos as u64;
        let mut zstream = ZStreamWrapper::new();
        zstream.init(DecompressionMode::RAW)?;

        let input_buffer = Box::new([0u8; BUFSIZE]);

        // Set the decompression dictionary FIRST (before any inflation)
        if checkpoint.plain_pos > 0 {
            zstream.set_dictionary(&checkpoint.window)?;
        }

        // Handle bit-level alignment
        if checkpoint.bits > 0 {
            // We're starting mid-byte. Read that byte and prime the bit buffer.
            let mut last_byte = [0u8; 1];
            file.read_exact(&mut last_byte)?;
            file_offset += 1;

            // Use inflatePrime exactly as the C++ code does:
            let bit_value = (last_byte[0] >> (8 - checkpoint.bits)) as i32;
            zstream.prime(checkpoint.bits as i32, bit_value)?;
        }

        Ok(GzipStreamReader {
            file,
            zstream,
            uncompressed_offset,
            input_buffer,
            file_offset,
            mode: DecompressionMode::RAW,
        })
    }
    /// Get current uncompressed offset
    pub fn uncompressed_offset(&self) -> u64 {
        self.uncompressed_offset
    }
}

impl io::Read for GzipStreamReader {
    /// Read decompressed data from the stream
    /// Handles multi-member gzip files (including BGZF)
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let len = buffer.len();
        let mut total_copied = 0;

        while total_copied < len {
            let remaining = len - total_copied;

            let strm = self.zstream.get_mut();

            if strm.avail_in == 0 {
                let bytes_read = self.file.read(&mut self.input_buffer[..])?;
                if bytes_read == 0 {
                    return Ok(total_copied);
                }
                self.file_offset += bytes_read as u64;
                strm.avail_in = bytes_read as u32;
                strm.next_in = self.input_buffer.as_mut_ptr();
            }

            strm.next_out = buffer[total_copied..].as_mut_ptr();
            strm.avail_out = remaining as u32;

            let in_before = strm.avail_in as i64;
            let out_before = strm.avail_out as i64;

            // WAS: Z_NO_FLUSH
            let ret = unsafe { libz_rs_sys::inflate(strm, libz_rs_sys::Z_BLOCK) };
            let in_after = strm.avail_in as i64;
            let out_after = strm.avail_out as i64;
            let consumed = in_before - in_after;
            let produced = out_before - out_after;

            total_copied += produced as usize;
            self.uncompressed_offset += produced as u64;

            if ret == Z_OK || ret == Z_BUF_ERROR {
                continue;
            } else if ret == Z_STREAM_END {
                trace!(
                    "Z_STREAM_END detected: avail_in={}, beg={}, totout={}",
                    strm.avail_in, in_after, out_after
                );

                if self.mode == DecompressionMode::RAW {
                    if strm.avail_in == 0 {
                        let bytes_read = self.file.read(&mut self.input_buffer[..])?;
                        if bytes_read == 0 {
                            return Ok(total_copied);
                        }
                        self.file_offset += bytes_read as u64;
                        strm.avail_in = bytes_read as u32;
                        strm.next_in = self.input_buffer.as_mut_ptr();
                    }

                    // Try skipping the 8-byte CRC, and then continue parsing in GZIP mode.
                    assert!(strm.avail_in >= 8);
                    strm.avail_in -= 8;
                    strm.next_in = unsafe { strm.next_in.add(8) };
                    if strm.avail_in == 0 {
                        return Ok(total_copied);
                    }
                    self.mode = detect_mode(
                        &self.input_buffer[self.input_buffer.len() - strm.avail_in as usize..],
                    );
                }

                let ret =
                    unsafe { libz_rs_sys::inflateReset2(self.zstream.stream, self.mode as i32) };
                if ret != Z_OK {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("inflate error: {}", ret),
                    ));
                }
                continue;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("inflate error: {}", ret),
                ));
            }
        }

        Ok(total_copied)
    }
}
// Example usage
pub fn example_usage(gz_file: &Path, index: &MimIndex, checkpoint_idx: usize) -> io::Result<()> {
    let mut reader = GzipStreamReader::open_at_checkpoint(gz_file, index, checkpoint_idx)?;

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
