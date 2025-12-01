use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::mem;
use std::path::Path;
use std::ptr;

use crate::mim_types::DeflateIndex;

// Direct zlib FFI bindings
#[repr(C)]
struct ZStream {
    next_in: *mut u8,
    avail_in: u32,
    total_in: u64,
    next_out: *mut u8,
    avail_out: u32,
    total_out: u64,
    msg: *mut libc::c_char,
    state: *mut libc::c_void,
    zalloc: *mut libc::c_void,
    zfree: *mut libc::c_void,
    opaque: *mut libc::c_void,
    data_type: i32,
    adler: u64,
    reserved: u64,
}

const Z_OK: i32 = 0;
const Z_STREAM_END: i32 = 1;
const Z_NEED_DICT: i32 = 2;
const Z_ERRNO: i32 = -1;
const Z_STREAM_ERROR: i32 = -2;
const Z_DATA_ERROR: i32 = -3;
const Z_MEM_ERROR: i32 = -4;
const Z_BUF_ERROR: i32 = -5;
const Z_VERSION_ERROR: i32 = -6;

const Z_NO_FLUSH: i32 = 0;

unsafe extern "C" {
    fn inflateInit2_(
        strm: *mut ZStream,
        window_bits: i32,
        version: *const libc::c_char,
        stream_size: i32,
    ) -> i32;
    fn inflate(strm: *mut ZStream, flush: i32) -> i32;
    fn inflateEnd(strm: *mut ZStream) -> i32;
    fn inflateReset(strm: *mut ZStream) -> i32;
    fn inflateSetDictionary(strm: *mut ZStream, dictionary: *const u8, dict_length: u32) -> i32;
    fn inflatePrime(strm: *mut ZStream, bits: i32, value: i32) -> i32;
}

// ZLIB_VERSION constant for inflateInit2_
const ZLIB_VERSION: &[u8] = b"1.2.11\0";

/// Safe wrapper around zlib's z_stream
struct ZStreamWrapper {
    stream: ZStream,
    initialized: bool,
}

impl ZStreamWrapper {
    fn new() -> Self {
        ZStreamWrapper {
            stream: unsafe { mem::zeroed() },
            initialized: false,
        }
    }

    fn init(&mut self, window_bits: i32) -> io::Result<()> {
        if self.initialized {
            unsafe { inflateEnd(&mut self.stream) };
        }

        let ret = unsafe {
            inflateInit2_(
                &mut self.stream,
                window_bits,
                ZLIB_VERSION.as_ptr() as *const libc::c_char,
                mem::size_of::<ZStream>() as i32,
            )
        };

        if ret != Z_OK {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("inflateInit2 failed: {}", ret),
            ));
        }

        self.initialized = true;
        Ok(())
    }

    fn set_dictionary(&mut self, dict: &[u8]) -> io::Result<()> {
        let ret =
            unsafe { inflateSetDictionary(&mut self.stream, dict.as_ptr(), dict.len() as u32) };

        if ret != Z_OK {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("inflateSetDictionary failed: {}", ret),
            ));
        }

        Ok(())
    }

    fn prime(&mut self, bits: i32, value: i32) -> io::Result<()> {
        let ret = unsafe { inflatePrime(&mut self.stream, bits, value) };

        if ret != Z_OK {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("inflatePrime failed: {}", ret),
            ));
        }

        Ok(())
    }

    fn reset(&mut self) -> io::Result<()> {
        let ret = unsafe { inflateReset(&mut self.stream) };

        if ret != Z_OK {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("inflateReset failed: {}", ret),
            ));
        }

        Ok(())
    }

    fn get_mut(&mut self) -> &mut ZStream {
        &mut self.stream
    }
}

impl Drop for ZStreamWrapper {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                inflateEnd(&mut self.stream);
            }
        }
    }
}

/// Reader for streaming decompression from a gzip checkpoint
pub struct GzipStreamReader {
    file: File,
    zstream: ZStreamWrapper,
    uncompressed_offset: u64,

    // Input buffer for compressed data
    input_buffer: Box<[u8; 131072]>, // 128KB

    // Output buffer for decompressed data
    output_buffer: Box<[u8; 131072]>, // 128KB
    output_buffer_size: usize,
    output_buffer_pos: usize,

    file_offset: u64,
    multi_member: bool,
    file_buffer: Option<Box<[u8]>>,
}

impl GzipStreamReader {
    /// Open a gzip file at a specific checkpoint
    pub fn open_at_checkpoint(
        gz_file_path: &Path,
        index: &DeflateIndex,
        checkpoint_index: usize,
    ) -> io::Result<Self> {
        if checkpoint_index >= index.have as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid checkpoint index",
            ));
        }

        let mut file = File::open(gz_file_path)?;

        // Get the checkpoint
        let checkpoint = &index.list[checkpoint_index];

        let uncompressed_offset = checkpoint.out as u64;

        // Seek to the compressed position
        let seek_pos = if checkpoint.bits > 0 {
            checkpoint.in_offset - 1
        } else {
            checkpoint.in_offset
        };
        file.seek(SeekFrom::Start(seek_pos as u64))?;
        let mut file_offset = seek_pos as u64;

        // Initialize decompressor in raw deflate mode (-15)
        let mut zstream = ZStreamWrapper::new();
        zstream.init(-15)?;

        // Set the decompression dictionary if we're not at the beginning
        if checkpoint.out > 0 {
            zstream.set_dictionary(&checkpoint.window)?;
        }

        let mut input_buffer = Box::new([0u8; 131072]);
        let mut output_buffer = Box::new([0u8; 131072]);

        // Handle bit-level alignment
        if checkpoint.bits > 0 {
            // Read the partial byte
            let mut last_byte = [0u8; 1];
            file.read_exact(&mut last_byte)?;
            file_offset += 1;

            // Prime the bit buffer with the unused bits
            let bit_value = (last_byte[0] >> (8 - checkpoint.bits)) as i32;
            zstream.prime(checkpoint.bits, bit_value)?;

            // Read initial data for decompression
            let bytes_read = file.read(&mut input_buffer[..])?;
            file_offset += bytes_read as u64;

            let strm = zstream.get_mut();
            strm.avail_in = bytes_read as u32;
            strm.next_in = input_buffer.as_mut_ptr();
        } else {
            let strm = zstream.get_mut();
            strm.avail_in = 0;
            strm.next_in = ptr::null_mut();
        }

        // Set up large FILE buffer for performance
        let file_buffer_size = 256 * 1024; // 256KB
        let file_buffer = vec![0u8; file_buffer_size].into_boxed_slice();

        Ok(GzipStreamReader {
            file,
            zstream,
            uncompressed_offset,
            input_buffer,
            output_buffer,
            output_buffer_size: 0,
            output_buffer_pos: 0,
            file_offset,
            multi_member: true,
            file_buffer: Some(file_buffer),
        })
    }

    /// Read decompressed data from the stream
    /// Handles multi-member gzip files (including BGZF)
    pub fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        let len = buffer.len();
        let mut total_copied = 0;
        let direct_threshold = self.output_buffer.len();

        while total_copied < len {
            let remaining = len - total_copied;

            // If we have decompressed data in output buffer, copy it
            if self.output_buffer_pos < self.output_buffer_size {
                let available = self.output_buffer_size - self.output_buffer_pos;
                let to_copy = remaining.min(available);

                buffer[total_copied..total_copied + to_copy].copy_from_slice(
                    &self.output_buffer[self.output_buffer_pos..self.output_buffer_pos + to_copy],
                );

                self.output_buffer_pos += to_copy;
                total_copied += to_copy;
                self.uncompressed_offset += to_copy as u64;
                continue;
            }

            // For large requests, decompress directly into user buffer
            if remaining >= direct_threshold {
                // Scope the strm borrow
                let ret = {
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

                    let ret = unsafe { inflate(strm, Z_NO_FLUSH) };
                    let produced = remaining - strm.avail_out as usize;

                    total_copied += produced;
                    self.uncompressed_offset += produced as u64;

                    ret
                }; // strm borrow ends here

                if ret == Z_OK || ret == Z_BUF_ERROR {
                    continue;
                } else if ret == Z_STREAM_END {
                    // Now we can call other methods on self
                    let avail_in = self.zstream.get_mut().avail_in;
                    if !self.multi_member || (avail_in == 0 && self.is_eof()?) {
                        return Ok(total_copied);
                    }

                    if !self.skip_gzip_header()? {
                        return Ok(total_copied);
                    }

                    self.zstream.reset()?;
                    continue;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("inflate error: {}", ret),
                    ));
                }
            }

            // Small request: decompress into internal buffer
            self.output_buffer_pos = 0;
            self.output_buffer_size = 0;

            while self.output_buffer_size < self.output_buffer.len() {
                // Scope the strm borrow
                let ret = {
                    let strm = self.zstream.get_mut();

                    if strm.avail_in == 0 {
                        let bytes_read = self.file.read(&mut self.input_buffer[..])?;
                        if bytes_read == 0 {
                            if self.output_buffer_size > 0 {
                                break;
                            }
                            return Ok(total_copied);
                        }
                        self.file_offset += bytes_read as u64;
                        strm.avail_in = bytes_read as u32;
                        strm.next_in = self.input_buffer.as_mut_ptr();
                    }

                    strm.next_out =
                        unsafe { self.output_buffer.as_mut_ptr().add(self.output_buffer_size) };
                    strm.avail_out = (self.output_buffer.len() - self.output_buffer_size) as u32;

                    let ret = unsafe { inflate(strm, Z_NO_FLUSH) };
                    let produced = (self.output_buffer.len() - self.output_buffer_size)
                        - strm.avail_out as usize;
                    self.output_buffer_size += produced;

                    ret
                }; // strm borrow ends here

                if ret == Z_OK || ret == Z_BUF_ERROR {
                    continue;
                } else if ret == Z_STREAM_END {
                    // Now we can call other methods on self
                    let avail_in = self.zstream.get_mut().avail_in;
                    if !self.multi_member || (avail_in == 0 && self.is_eof()?) {
                        break;
                    }

                    if !self.skip_gzip_header()? {
                        break;
                    }

                    self.zstream.reset()?;
                    continue;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("inflate error: {}", ret),
                    ));
                }
            }
        }

        Ok(total_copied)
    }

    /// Check if at EOF
    fn is_eof(&mut self) -> io::Result<bool> {
        let mut byte = [0u8; 1];
        match self.file.read(&mut byte)? {
            0 => Ok(true),
            1 => {
                // Put it back by seeking backwards
                self.file.seek(SeekFrom::Current(-1))?;
                Ok(false)
            }
            _ => unreachable!(),
        }
    }

    /// Skip gzip header and trailer
    fn skip_gzip_header(&mut self) -> io::Result<bool> {
        // Skip 8-byte trailer
        {
            let strm = self.zstream.get_mut();
            if strm.avail_in >= 8 {
                strm.next_in = unsafe { strm.next_in.add(8) };
                strm.avail_in -= 8;
            } else {
                let to_skip = 8 - strm.avail_in as usize;
                strm.avail_in = 0;
                let mut discard = vec![0u8; to_skip];
                if self.file.read_exact(&mut discard).is_err() {
                    return Ok(false);
                }
                self.file_offset += to_skip as u64;
            }
        } // Drop strm borrow

        self.ensure_bytes(10)?;

        // Validate header
        let (id1, id2, cm, flags) = {
            let strm = self.zstream.get_mut();
            if strm.avail_in < 10 {
                return Ok(false);
            }

            let header = unsafe { std::slice::from_raw_parts(strm.next_in, 10) };
            if header[0] != 0x1f || header[1] != 0x8b || header[2] != 8 {
                return Ok(false);
            }

            let flags = header[3];
            strm.next_in = unsafe { strm.next_in.add(10) };
            strm.avail_in -= 10;

            (header[0], header[1], header[2], flags)
        }; // Drop strm borrow

        // FEXTRA
        if flags & 0x04 != 0 {
            self.ensure_bytes(2)?;

            let extra_len = {
                let strm = self.zstream.get_mut();
                if strm.avail_in < 2 {
                    return Ok(false);
                }

                let bytes = unsafe { std::slice::from_raw_parts(strm.next_in, 2) };
                let extra_len = bytes[0] as usize | ((bytes[1] as usize) << 8);
                strm.next_in = unsafe { strm.next_in.add(2) };
                strm.avail_in -= 2;
                extra_len
            }; // Drop strm borrow

            self.ensure_bytes(extra_len)?;

            {
                let strm = self.zstream.get_mut();
                if strm.avail_in < extra_len as u32 {
                    return Ok(false);
                }
                strm.next_in = unsafe { strm.next_in.add(extra_len) };
                strm.avail_in -= extra_len as u32;
            } // Drop strm borrow
        }

        // FNAME
        if flags & 0x08 != 0 {
            loop {
                let (avail_in, is_zero) = {
                    let strm = self.zstream.get_mut();
                    (
                        strm.avail_in,
                        if strm.avail_in > 0 {
                            unsafe { *strm.next_in == 0 }
                        } else {
                            false
                        },
                    )
                };

                if avail_in == 0 {
                    let bytes_read = self.file.read(&mut self.input_buffer[..])?;
                    if bytes_read == 0 {
                        return Ok(false);
                    }
                    self.file_offset += bytes_read as u64;
                    let strm = self.zstream.get_mut();
                    strm.avail_in = bytes_read as u32;
                    strm.next_in = self.input_buffer.as_mut_ptr();
                    continue;
                }

                let strm = self.zstream.get_mut();
                if is_zero {
                    strm.next_in = unsafe { strm.next_in.add(1) };
                    strm.avail_in -= 1;
                    break;
                }
                strm.next_in = unsafe { strm.next_in.add(1) };
                strm.avail_in -= 1;
            }
        }

        // FCOMMENT
        if flags & 0x10 != 0 {
            loop {
                let (avail_in, is_zero) = {
                    let strm = self.zstream.get_mut();
                    (
                        strm.avail_in,
                        if strm.avail_in > 0 {
                            unsafe { *strm.next_in == 0 }
                        } else {
                            false
                        },
                    )
                };

                if avail_in == 0 {
                    let bytes_read = self.file.read(&mut self.input_buffer[..])?;
                    if bytes_read == 0 {
                        return Ok(false);
                    }
                    self.file_offset += bytes_read as u64;
                    let strm = self.zstream.get_mut();
                    strm.avail_in = bytes_read as u32;
                    strm.next_in = self.input_buffer.as_mut_ptr();
                    continue;
                }

                let strm = self.zstream.get_mut();
                if is_zero {
                    strm.next_in = unsafe { strm.next_in.add(1) };
                    strm.avail_in -= 1;
                    break;
                }
                strm.next_in = unsafe { strm.next_in.add(1) };
                strm.avail_in -= 1;
            }
        }

        // FHCRC
        if flags & 0x02 != 0 {
            self.ensure_bytes(2)?;

            let strm = self.zstream.get_mut();
            if strm.avail_in < 2 {
                return Ok(false);
            }
            strm.next_in = unsafe { strm.next_in.add(2) };
            strm.avail_in -= 2;
        }

        Ok(true)
    }

    /// Ensure buffer has at least needed bytes
    fn ensure_bytes(&mut self, needed: usize) -> io::Result<()> {
        let strm = self.zstream.get_mut();
        let available = strm.avail_in as usize;

        if available < needed {
            // Move remaining data to start
            if available > 0 {
                unsafe {
                    ptr::copy(strm.next_in, self.input_buffer.as_mut_ptr(), available);
                }
            }

            let bytes_read = self.file.read(&mut self.input_buffer[available..])?;
            self.file_offset += bytes_read as u64;
            strm.avail_in = (available + bytes_read) as u32;
            strm.next_in = self.input_buffer.as_mut_ptr();
        }

        Ok(())
    }

    /// Set whether to check for multi-member gzip files
    pub fn set_multi_member(&mut self, multi_member: bool) {
        self.multi_member = multi_member;
    }

    /// Get current uncompressed offset
    pub fn uncompressed_offset(&self) -> u64 {
        self.uncompressed_offset
    }
}

// Example usage
pub fn example_usage(
    gz_file: &Path,
    index: &DeflateIndex,
    checkpoint_idx: usize,
) -> io::Result<()> {
    let mut reader = GzipStreamReader::open_at_checkpoint(gz_file, index, checkpoint_idx)?;

    // Optionally disable multi-member checking for single-member files
    // reader.set_multi_member(false);

    let mut buffer = vec![0u8; 131072]; // 128KB
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
