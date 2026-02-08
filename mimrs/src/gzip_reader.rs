use crate::mim_types::MimIndex;
use libz_rs_sys::{
    Z_BUF_ERROR, Z_NO_FLUSH, Z_OK, Z_STREAM_END, inflate, inflateEnd, inflatePrime, inflateReset,
    inflateSetDictionary, z_stream, zlibVersion,
};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::ptr;

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

    fn init(&mut self, window_bits: i32) -> io::Result<()> {
        if self.initialized {
            unsafe { inflateEnd(self.stream) };
        }

        // Zero out the memory
        unsafe {
            ptr::write_bytes(self.stream, 0, 1);
        }

        let ret = unsafe {
            let version = zlibVersion();
            libz_rs_sys::inflateInit2_(
                self.stream,
                window_bits,
                version,
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
        let _state_ptr = unsafe { (*self.stream).state };
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

    fn reset(&mut self) -> io::Result<()> {
        let ret = unsafe { inflateReset(self.stream) };

        if ret != Z_OK {
            return Err(io::Error::other(format!("inflateReset failed: {}", ret)));
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

    // Input buffer for compressed data
    input_buffer: Box<[u8; BUFSIZE]>, // 128KB

    file_offset: u64,
    multi_member: bool,
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

        /*
        const size_t file_buffer_size = 256 * 1024;  // 256KB
        reader->file_buffer = new char[file_buffer_size];
        if (setvbuf(gz_file, reader->file_buffer, _IOFBF, file_buffer_size) != 0) {
            // setvbuf failed, continue without custom buffer (will hurt performance)
            delete[] reader->file_buffer;
            reader->file_buffer = nullptr;
        }
        */

        // Get the checkpoint
        let checkpoint = &index.checkpoints[checkpoint_index];

        let uncompressed_offset = checkpoint.plain_pos as u64;
        // Seek to the compressed position
        let seek_pos = if checkpoint.bits > 0 {
            checkpoint.gz_pos - 1
        } else {
            checkpoint.gz_pos
        };

        file.seek(SeekFrom::Start(seek_pos as u64))?;
        let mut file_offset = seek_pos as u64;

        // Initialize decompressor in raw deflate mode (-15)
        // Always use raw deflate when starting from a checkpoint
        let mut zstream = ZStreamWrapper::new();
        zstream.init(-15)?;

        let mut input_buffer = Box::new([0u8; BUFSIZE]);

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

            // Now read the next chunk of data into our buffer for inflate to use
            let bytes_read = file.read(&mut input_buffer[..])?;

            if bytes_read > 0 {
                file_offset += bytes_read as u64;
                let strm = zstream.get_mut();
                strm.avail_in = bytes_read as u32;
                strm.next_in = input_buffer.as_mut_ptr();
            } else {
                let strm = zstream.get_mut();
                strm.avail_in = 0;
                strm.next_in = ptr::null_mut();
            }
        } else {
            // No bit alignment needed, start with clean byte boundary
            let strm = zstream.get_mut();
            strm.avail_in = 0;
            strm.next_in = ptr::null_mut();
        }

        // Set up large FILE buffer for performance
        //let file_buffer_size = 256 * 1024; // 256KB
        //let file_buffer = vec![0u8; file_buffer_size].into_boxed_slice();

        Ok(GzipStreamReader {
            file,
            zstream,
            uncompressed_offset,
            input_buffer,
            file_offset,
            multi_member: true,
            //file_buffer: Some(file_buffer),
        })
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
        let flags = {
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

            flags
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

impl io::Read for GzipStreamReader {
    /// Read decompressed data from the stream
    /// Handles multi-member gzip files (including BGZF)
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        let len = buffer.len();
        let mut total_copied = 0;

        while total_copied < len {
            let remaining = len - total_copied;

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

        Ok(total_copied)
    }
}
// Example usage
pub fn example_usage(gz_file: &Path, index: &MimIndex, checkpoint_idx: usize) -> io::Result<()> {
    let mut reader = GzipStreamReader::open_at_checkpoint(gz_file, index, checkpoint_idx)?;

    // Optionally disable multi-member checking for single-member files
    // reader.set_multi_member(false);

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
