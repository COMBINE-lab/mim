#ifndef ZRAN_INDEX_HELPERS_HPP
#define ZRAN_INDEX_HELPERS_HPP

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <zlib.h>
#include "zran.hpp"
#include <fcntl.h>       // For open(), O_RDONLY
#include <unistd.h>      // For lseek(), read(), close()
#include <cstring>      // For memset(), strdup()
#include <cstdlib>      // For malloc(), free()
#include <stdexcept>     // For std::runtime_error, std::invalid_argum

// RAII wrapper for z_stream
class ZStream {
public:
    ZStream() {
        memset(&stream_, 0, sizeof(stream_));
        stream_.zalloc = Z_NULL;
        stream_.zfree = Z_NULL;
        stream_.opaque = Z_NULL;
        stream_.avail_in = 0;
        stream_.next_in = Z_NULL;
        initialized_ = false;
    }
    
    ~ZStream() {
        cleanup();
    }
    
    // Delete copy operations - z_stream cannot be safely copied
    ZStream(const ZStream&) = delete;
    ZStream& operator=(const ZStream&) = delete;
    
    // Move constructor
    ZStream(ZStream&& other) noexcept 
        : stream_(other.stream_), initialized_(other.initialized_) {
        other.initialized_ = false;
        // Zero out the moved-from stream to prevent double-free
        memset(&other.stream_, 0, sizeof(other.stream_));
    }
    
    // Move assignment
    ZStream& operator=(ZStream&& other) noexcept {
        if (this != &other) {
            // Clean up our current resources
            cleanup();
            
            // Take ownership of other's resources
            stream_ = other.stream_;
            initialized_ = other.initialized_;
            
            // Leave other in a valid but empty state
            other.initialized_ = false;
            memset(&other.stream_, 0, sizeof(other.stream_));
        }
        return *this;
    }
    
    z_stream* get() { return &stream_; }
    
    void init(int windowBits) {
        if (initialized_) {
            inflateEnd(&stream_);
        }
        int ret = inflateInit2(&stream_, windowBits);
        if (ret != Z_OK) {
            throw std::runtime_error("Failed to initialize inflate stream");
        }
        initialized_ = true;
    }
    
    void setDictionary(const unsigned char* dict, size_t dictLen) {
        int ret = inflateSetDictionary(&stream_, dict, dictLen);
        if (ret != Z_OK) {
            throw std::runtime_error("Failed to set dictionary");
        }
    }
    
private:
    z_stream stream_;
    bool initialized_;
    
    void cleanup() {
        if (initialized_) {
            inflateEnd(&stream_);
            initialized_ = false;
        }
    }
};

/**
 * State for reading from a gzip file starting at a checkpoint
 */
struct GzipStreamReader {
    FILE* file;                    // The opened gzip file
    struct deflate_index* index;   // The index (caller owns, must free)
    ZStream zstream;               // Decompression stream (managed internally)
    off_t uncompressed_offset;     // Current position in uncompressed data
    
    // Input buffer for compressed data
    unsigned char input_buffer[131072];  // 128KB input buffer (matching gzread default)
    size_t input_buffer_size;           // Valid bytes in input buffer
    size_t input_buffer_pos;            // Current position in input buffer
    
    unsigned char output_buffer[131072]; // 128KB output buffer
    size_t output_buffer_size;           // Valid decompressed bytes in output buffer
    size_t output_buffer_pos;            // Current read position in output buffer
    
    off_t file_offset;                  // Current position in file
    int bits_from_last_byte;            // Bits pending from previous byte
    bool multi_member;                  // Whether to check for multiple members
    char* file_buffer;                  // Buffer for FILE* (owned by us)
    
    // Default constructor
    GzipStreamReader() 
        : file(nullptr),
          index(nullptr),
          zstream(),
          uncompressed_offset(0),
          input_buffer_size(0),
          input_buffer_pos(0),
          output_buffer_size(0),
          output_buffer_pos(0),
          file_offset(0),
          bits_from_last_byte(0),
          multi_member(true),
          file_buffer(nullptr)
    {
        memset(input_buffer, 0, sizeof(input_buffer));
        memset(output_buffer, 0, sizeof(output_buffer));
    }
    
    ~GzipStreamReader() {
        if (file) fclose(file);
        delete[] file_buffer;
        // index is not owned by us, don't free it
    }
    
    // Delete copy operations - cannot safely copy FILE* and z_stream
    GzipStreamReader(const GzipStreamReader&) = delete;
    GzipStreamReader& operator=(const GzipStreamReader&) = delete;
    
    // Move constructor
    GzipStreamReader(GzipStreamReader&& other) noexcept
        : file(other.file),
          index(other.index),
          zstream(std::move(other.zstream)),
          uncompressed_offset(other.uncompressed_offset),
          input_buffer_size(other.input_buffer_size),
          input_buffer_pos(other.input_buffer_pos),
          file_offset(other.file_offset),
          bits_from_last_byte(other.bits_from_last_byte)
    {
        // Copy the input buffer
        memcpy(input_buffer, other.input_buffer, sizeof(input_buffer));
        
        // Null out the moved-from object's file pointer
        other.file = nullptr;
        other.index = nullptr;
    }
    
    // Move assignment
    GzipStreamReader& operator=(GzipStreamReader&& other) noexcept {
        if (this != &other) {
            // Clean up our current resources
            if (file) fclose(file);
            
            // Take ownership of other's resources
            file = other.file;
            index = other.index;
            zstream = std::move(other.zstream);
            uncompressed_offset = other.uncompressed_offset;
            input_buffer_size = other.input_buffer_size;
            input_buffer_pos = other.input_buffer_pos;
            file_offset = other.file_offset;
            bits_from_last_byte = other.bits_from_last_byte;
            
            // Copy the input buffer
            memcpy(input_buffer, other.input_buffer, sizeof(input_buffer));
            
            // Null out the moved-from object
            other.file = nullptr;
            other.index = nullptr;
        }
        return *this;
    }
};

/**
 * Enhanced version of open_gzip_at_checkpoint that returns a GzipStreamReader
 * with full multi-member support. This uses your existing GzipStreamReader struct.
 */
std::unique_ptr<GzipStreamReader> open_gzip_stream_at_checkpoint(
    const char* gz_file_path,
    struct deflate_index* index,
    size_t checkpoint_index)
{
    
    if (!index || checkpoint_index >= static_cast<size_t>(index->have)) {
        throw std::invalid_argument("Invalid checkpoint index");
    }
    
    // Open the gzip file
    FILE* gz_file = fopen(gz_file_path, "rb");
    if (!gz_file) {
        throw std::runtime_error(std::string("Failed to open gzip file: ") + gz_file_path);
    }
    
    auto reader = std::make_unique<GzipStreamReader>();
    reader->file = gz_file;
    reader->index = index;
    reader->input_buffer_size = 0;
    reader->input_buffer_pos = 0;
    
    const size_t file_buffer_size = 256 * 1024;  // 256KB 
    reader->file_buffer = new char[file_buffer_size];
    if (setvbuf(gz_file, reader->file_buffer, _IOFBF, file_buffer_size) != 0) {
        // setvbuf failed, continue without custom buffer (will hurt performance)
        delete[] reader->file_buffer;
        reader->file_buffer = nullptr;
    }
    
    // Get the checkpoint
    struct point* checkpoint = &(*index->list)[checkpoint_index];
    
    reader->uncompressed_offset = checkpoint->out;
    reader->bits_from_last_byte = checkpoint->bits;
    
    // Seek to the compressed position
    if (fseeko(gz_file, checkpoint->in - (checkpoint->bits ? 1 : 0), SEEK_SET) != 0) {
        throw std::runtime_error("Failed to seek in gzip file");
    }
    reader->file_offset = checkpoint->in - (checkpoint->bits ? 1 : 0);
    
    // Initialize the inflate stream
    // Always use raw deflate when starting from a checkpoint, even checkpoint 0
    // The checkpoint already accounts for any gzip headers
    int windowBits = -15;  // Raw deflate (no wrapper)
    reader->zstream.init(windowBits);
    
    z_stream* strm = reader->zstream.get();
    
    // Set the decompression dictionary (the 32KB window from the checkpoint)
    if (checkpoint->out > 0) {
        reader->zstream.setDictionary(checkpoint->window, 32768);
    }
    
    // Handle bit-level alignment
    if (checkpoint->bits) {
        // We're starting mid-byte. Read that byte and prime the bit buffer.
        unsigned char last_byte;
        if (fread(&last_byte, 1, 1, gz_file) != 1) {
            throw std::runtime_error("Failed to read alignment byte");
        }
        reader->file_offset++;
        
        // Use inflatePrime exactly as zran.c does:
        // Pass checkpoint->bits (the number of bits ALREADY used)
        // and shift right by (8 - checkpoint->bits) to get the unused portion
        int ret = inflatePrime(strm, checkpoint->bits, last_byte >> (8 - checkpoint->bits));
        if (ret != Z_OK) {
            throw std::runtime_error("Failed to prime inflate stream");
        }
        
        // Now read the next chunk of data into our buffer for inflate to use
        size_t bytes_read = fread(reader->input_buffer, 1, sizeof(reader->input_buffer), gz_file);
        if (bytes_read > 0) {
            reader->file_offset += bytes_read;
            strm->avail_in = bytes_read;
            strm->next_in = reader->input_buffer;
        } else {
            strm->avail_in = 0;
            strm->next_in = Z_NULL;
        }
    } else {
        // No bit alignment needed, start with clean byte boundary
        strm->avail_in = 0;
        strm->next_in = Z_NULL;
    }
    
    return reader;
}

/**
 * Skip gzip header and trailer to get to the next gzip member.
 * Returns 0 on success, -1 on error/EOF.
 */
static int skip_gzip_header(z_stream* strm, FILE* file, unsigned char* input_buffer, 
                           size_t buffer_size, off_t* file_offset) {
    // Skip 8-byte gzip trailer (CRC32 + ISIZE)
    if (strm->avail_in >= 8) {
        strm->next_in += 8;
        strm->avail_in -= 8;
    } else {
        size_t to_skip = 8 - strm->avail_in;
        strm->avail_in = 0;
        unsigned char discard[8];
        if (fread(discard, 1, to_skip, file) != to_skip) {
            return -1;
        }
        *file_offset += to_skip;
    }
    
    // Ensure we have at least 10 bytes for the gzip header
    if (strm->avail_in < 10) {
        if (strm->avail_in > 0 && strm->next_in != input_buffer) {
            memmove(input_buffer, strm->next_in, strm->avail_in);
        }
        size_t bytes_read = fread(input_buffer + strm->avail_in, 1, 
                                  buffer_size - strm->avail_in, file);
        if (bytes_read == 0 && strm->avail_in < 10) {
            return -1;
        }
        *file_offset += bytes_read;
        strm->avail_in += bytes_read;
        strm->next_in = input_buffer;
    }
    
    if (strm->avail_in < 10) return -1;
    
    // Validate gzip header
    unsigned char* header = strm->next_in;
    if (header[0] != 0x1f || header[1] != 0x8b || header[2] != 8) {
        return -1;
    }
    
    unsigned char flags = header[3];
    strm->next_in += 10;
    strm->avail_in -= 10;
    
    // Helper lambda to ensure we have enough data in the buffer
    auto ensure_bytes = [&](size_t needed) -> bool {
        if (strm->avail_in < needed) {
            if (strm->avail_in > 0 && strm->next_in != input_buffer) {
                memmove(input_buffer, strm->next_in, strm->avail_in);
            }
            size_t bytes_read = fread(input_buffer + strm->avail_in, 1, 
                                      buffer_size - strm->avail_in, file);
            *file_offset += bytes_read;
            strm->avail_in += bytes_read;
            strm->next_in = input_buffer;
            return strm->avail_in >= needed;
        }
        return true;
    };
    
    // FEXTRA - extra fields (used by BGZF)
    if (flags & 0x04) {
        if (!ensure_bytes(2)) return -1;
        size_t extra_len = strm->next_in[0] | (strm->next_in[1] << 8);
        strm->next_in += 2;
        strm->avail_in -= 2;
        
        if (!ensure_bytes(extra_len)) return -1;
        strm->next_in += extra_len;
        strm->avail_in -= extra_len;
    }
    
    // FNAME - null-terminated filename
    if (flags & 0x08) {
        while (strm->avail_in > 0 && *strm->next_in != 0) {
            strm->next_in++;
            strm->avail_in--;
        }
        if (strm->avail_in == 0) {
            size_t bytes_read = fread(input_buffer, 1, buffer_size, file);
            if (bytes_read == 0) return -1;
            *file_offset += bytes_read;
            strm->avail_in = bytes_read;
            strm->next_in = input_buffer;
        }
        strm->next_in++;
        strm->avail_in--;
    }
    
    // FCOMMENT - null-terminated comment
    if (flags & 0x10) {
        while (strm->avail_in > 0 && *strm->next_in != 0) {
            strm->next_in++;
            strm->avail_in--;
        }
        if (strm->avail_in == 0) {
            size_t bytes_read = fread(input_buffer, 1, buffer_size, file);
            if (bytes_read == 0) return -1;
            *file_offset += bytes_read;
            strm->avail_in = bytes_read;
            strm->next_in = input_buffer;
        }
        strm->next_in++;
        strm->avail_in--;
    }
    
    // FHCRC - 2-byte header CRC
    if (flags & 0x02) {
        if (!ensure_bytes(2)) return -1;
        strm->next_in += 2;
        strm->avail_in -= 2;
    }
    
    return 0;
}

/**
 * Read decompressed data from the stream.
 * Handles multi-member gzip files (including BGZF).
 * Returns number of bytes read (0 = EOF, negative = error).
 * 
 * Optimized to match gzread's strategy:
 * - For small reads: use internal buffer and copy
 * - For large reads: decompress directly into user buffer
 */
ptrdiff_t gzipstream_read(GzipStreamReader* reader, char* buffer, size_t len) {
    if (!reader || !reader->file || len == 0) {
        return -1;
    }
    
    size_t total_copied = 0;
    z_stream* strm = reader->zstream.get();
    
    // Threshold: Use buffered path for small requests (matching zlib-ng logic)
    const size_t buffer_threshold = sizeof(reader->output_buffer);
    
    while (total_copied < len) {
        size_t remaining = len - total_copied;
        unsigned n = (remaining > UINT_MAX) ? UINT_MAX : (unsigned)remaining;
        
        // First: try to copy from output buffer if available
        if (reader->output_buffer_pos < reader->output_buffer_size) {
            unsigned available = reader->output_buffer_size - reader->output_buffer_pos;
            if (available < n)
                n = available;
            
            // Manual copy for small sizes (faster than memcpy)
            if (n <= 64) {
                unsigned char* dst = reinterpret_cast<unsigned char*>(buffer) + total_copied;
                unsigned char* src = reader->output_buffer + reader->output_buffer_pos;
                unsigned count = n;
                do {
                    *dst++ = *src++;
                } while (--count);
            } else {
                memcpy(buffer + total_copied, 
                       reader->output_buffer + reader->output_buffer_pos, 
                       n);
            }
            
            reader->output_buffer_pos += n;
            total_copied += n;
            reader->uncompressed_offset += n;
            continue;
        }
        
        // Check for end of file
        if (strm->avail_in == 0 && feof(reader->file)) {
            break;
        }
        
        // Small request OR need to look for headers: use buffered path
        if (n < buffer_threshold) {
            // Reset output buffer for new fill
            reader->output_buffer_pos = 0;
            reader->output_buffer_size = 0;
            
            // Fill the output buffer completely
            strm->next_out = reader->output_buffer;
            strm->avail_out = sizeof(reader->output_buffer);
            
            while (strm->avail_out > 0) {
                // Load more input if needed
                if (strm->avail_in == 0) {
                    size_t bytes_read = fread(reader->input_buffer, 1, 
                                             sizeof(reader->input_buffer), reader->file);
                    if (bytes_read == 0) {
                        break;  // EOF or error
                    }
                    reader->file_offset += bytes_read;
                    strm->avail_in = bytes_read;
                    strm->next_in = reader->input_buffer;
                }
                
                unsigned char* out_before = strm->next_out;
                int ret = inflate(strm, Z_NO_FLUSH);
                unsigned produced = strm->next_out - out_before;
                reader->output_buffer_size += produced;
                
                if (ret == Z_STREAM_END) {
                    if (!reader->multi_member) {
                        break;
                    }
                    
                    // Check if more data available
                    if (strm->avail_in == 0 && feof(reader->file)) {
                        break;
                    }
                    
                    // Process next member
                    if (skip_gzip_header(strm, reader->file, reader->input_buffer, 
                                       sizeof(reader->input_buffer), &reader->file_offset) != 0) {
                        break;
                    }
                    
                    if (inflateReset(strm) != Z_OK) {
                        return -1;
                    }
                } else if (ret != Z_OK && ret != Z_BUF_ERROR) {
                    return -1;
                }
                
                // If we got Z_BUF_ERROR with no input, we need more data
                if (ret == Z_BUF_ERROR && strm->avail_in == 0) {
                    break;
                }
            }
            
            // Loop back to copy from output buffer
            continue;
        }
        
        // Large request: decompress directly into user buffer
        // Load more compressed data if needed
        if (strm->avail_in == 0) {
            size_t bytes_read = fread(reader->input_buffer, 1, 
                                     sizeof(reader->input_buffer), reader->file);
            if (bytes_read == 0) {
                return total_copied > 0 ? total_copied : 0;
            }
            reader->file_offset += bytes_read;
            strm->avail_in = bytes_read;
            strm->next_in = reader->input_buffer;
        }
        
        // Decompress directly into user buffer
        strm->next_out = reinterpret_cast<unsigned char*>(buffer) + total_copied;
        strm->avail_out = n;
        
        unsigned char* out_before = strm->next_out;
        int ret = inflate(strm, Z_NO_FLUSH);
        unsigned produced = strm->next_out - out_before;
        
        total_copied += produced;
        reader->uncompressed_offset += produced;
        
        if (ret == Z_STREAM_END) {
            if (!reader->multi_member || (strm->avail_in == 0 && feof(reader->file))) {
                return total_copied;
            }
            
            if (skip_gzip_header(strm, reader->file, reader->input_buffer, 
                               sizeof(reader->input_buffer), &reader->file_offset) != 0) {
                return total_copied;
            }
            
            if (inflateReset(strm) != Z_OK) {
                return -1;
            }
        } else if (ret != Z_OK && ret != Z_BUF_ERROR) {
            return -1;
        }
    }
    
    return total_copied;
}

///
 // Read decompressed data from the stream.
 // Handles multi-member gzip files (including BGZF).
 // Returns number of bytes read (0 = EOF, negative = error).
 ///
inline ptrdiff_t gzipstream_read_good(GzipStreamReader* reader, char* buffer, size_t len) {
 if (!reader || !reader->file || len == 0) {
        return -1;
    }
    
    size_t total_copied = 0;
    
    while (total_copied < len) {
        // If we have decompressed data in our output buffer, copy it first
        if (reader->output_buffer_pos < reader->output_buffer_size) {
            size_t available = reader->output_buffer_size - reader->output_buffer_pos;
            size_t to_copy = (len - total_copied < available) ? (len - total_copied) : available;
            
            memcpy(buffer + total_copied, 
                   reader->output_buffer + reader->output_buffer_pos, 
                   to_copy);
            
            reader->output_buffer_pos += to_copy;
            total_copied += to_copy;
            reader->uncompressed_offset += to_copy;
            
            continue;
        }
        
        // Output buffer is empty, need to decompress more data
        // Reset output buffer for new decompression
        reader->output_buffer_pos = 0;
        reader->output_buffer_size = 0;
        
        z_stream* strm = reader->zstream.get();
        
        // Decompress a full buffer's worth into our output buffer
        while (reader->output_buffer_size < sizeof(reader->output_buffer)) {
            // Read more compressed data if needed
            if (strm->avail_in == 0) {
                size_t bytes_read = fread(reader->input_buffer, 1, sizeof(reader->input_buffer), reader->file);
                if (bytes_read == 0) {
                    // EOF or error - return what we've decompressed so far
                    if (reader->output_buffer_size > 0) {
                        goto copy_from_output;
                    }
                    return total_copied > 0 ? total_copied : 0;
                }
                reader->file_offset += bytes_read;
                strm->avail_in = bytes_read;
                strm->next_in = reader->input_buffer;
            }
            
            // Set up output to decompress into our internal output buffer
            strm->next_out = reader->output_buffer + reader->output_buffer_size;
            strm->avail_out = sizeof(reader->output_buffer) - reader->output_buffer_size;
            
            // Decompress
            int ret = inflate(strm, Z_NO_FLUSH);
            
            // Calculate how much was produced
            size_t produced = (sizeof(reader->output_buffer) - reader->output_buffer_size) - strm->avail_out;
            reader->output_buffer_size += produced;
            
            if (ret == Z_OK || ret == Z_BUF_ERROR) {
                // Continue decompressing
                continue;
            } else if (ret == Z_STREAM_END) {
                // End of gzip member
                if (!reader->multi_member) {
                    // Not checking for multi-member, done decompressing
                    goto copy_from_output;
                }
                
                // Check for another member
                if (strm->avail_in > 0 || !feof(reader->file)) {
                    if (skip_gzip_header(strm, reader->file, reader->input_buffer, 
                                       sizeof(reader->input_buffer), &reader->file_offset) != 0) {
                        goto copy_from_output;
                    }
                    
                    if (inflateReset(strm) != Z_OK) {
                        return -1;
                    }
                    continue;
                } else {
                    goto copy_from_output;
                }
            } else {
                // Error
                return -1;
            }
        }
        
copy_from_output:
        // We've filled the output buffer (or reached EOF/end of member)
        // Loop back to copy from it
        continue;
    }
    
    return total_copied;
}

#endif //ZRAN_INDEX_HELPERS_HPP
