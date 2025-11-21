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
    
    unsigned char input_buffer[4096];   // Buffer for compressed input
    size_t input_buffer_size;           // Valid bytes in input buffer
    size_t input_buffer_pos;            // Current position in input buffer
    off_t file_offset;                  // Current position in file
    int bits_from_last_byte;            // Bits pending from previous byte
    
    // Default constructor
    GzipStreamReader() 
        : file(nullptr),
          index(nullptr),
          zstream(),
          uncompressed_offset(0),
          input_buffer_size(0),
          input_buffer_pos(0),
          file_offset(0),
          bits_from_last_byte(0)
    {
        memset(input_buffer, 0, sizeof(input_buffer));
    }
    
    ~GzipStreamReader() {
        if (file) fclose(file);
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
 * Initialize the stream reader at a specific checkpoint.
 * This replicates what deflate_index_extract does, but keeps the z_stream alive.
 */
std::unique_ptr<GzipStreamReader> open_gzip_at_checkpoint(
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
    
    // Get the checkpoint
    struct point* checkpoint = &index->list->at(checkpoint_index);
   
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
 */
inline ptrdiff_t gzip_read(GzipStreamReader* reader, char* buffer, size_t len) {
    if (!reader || !reader->file) {
        return -1;
    }
    
    z_stream* strm = reader->zstream.get();
    strm->avail_out = len;
    strm->next_out = reinterpret_cast<unsigned char*>(buffer);
    
    while (strm->avail_out > 0) {
        // Read more compressed data if needed
        if (strm->avail_in == 0) {
            size_t bytes_read = fread(reader->input_buffer, 1, sizeof(reader->input_buffer), reader->file);
            if (bytes_read == 0) {
                if (feof(reader->file)) {
                    break;
                } else {
                    return -1;
                }
            }
            reader->file_offset += bytes_read;
            strm->avail_in = bytes_read;
            strm->next_in = reader->input_buffer;
        }
        
        // Decompress
        int ret = inflate(strm, Z_NO_FLUSH);
        
        if (ret == Z_STREAM_END) {
            // End of gzip member - check for another member
            if (strm->avail_in > 0 || !feof(reader->file)) {
                // Skip to next gzip member
                if (skip_gzip_header(strm, reader->file, reader->input_buffer, 
                                   sizeof(reader->input_buffer), &reader->file_offset) != 0) {
                    break;
                }
                
                // Reset inflate state for next member
                if (inflateReset(strm) != Z_OK) {
                    return -1;
                }
                continue;
            } else {
                break;
            }
        } else if (ret == Z_OK || ret == Z_BUF_ERROR) {
            continue;
        } else {
            return -1;
        }
    }
    
    size_t bytes_produced = len - strm->avail_out;
    reader->uncompressed_offset += bytes_produced;
    return bytes_produced;
}

/*
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
    ZStream(ZStream&& other) = delete; 
   
    // Move assignment
    ZStream& operator=(ZStream&& other) = delete; 

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
    void cleanup() {
        if (initialized_) {
            inflateEnd(&stream_);
            initialized_ = false;
        }
    }
private:
    z_stream stream_;
    bool initialized_;
};

//
// State for reading from a gzip file starting at a checkpoint
//
struct GzipStreamReader {
    FILE* file;                    // The opened gzip file
    struct deflate_index* index;   // The index (caller owns, must free)
    ZStream zstream;               // Decompression stream (managed internally)
    off_t uncompressed_offset;     // Current position in uncompressed data
    
    unsigned char input_buffer[4096];   // Buffer for compressed input
    size_t input_buffer_size;           // Valid bytes in input buffer
    size_t input_buffer_pos;            // Current position in input buffer
    off_t file_offset;                  // Current position in file
    int bits_from_last_byte;            // Bits pending from previous byte
    
    // Default constructor
    GzipStreamReader() 
        : file(nullptr),
          index(nullptr),
          zstream(),
          uncompressed_offset(0),
          input_buffer_size(0),
          input_buffer_pos(0),
          file_offset(0),
          bits_from_last_byte(0)
    {
        memset(input_buffer, 0, sizeof(input_buffer));
    }

    ~GzipStreamReader() {
        if (file) fclose(file);
        // index is not owned by us, don't free it
    }
    
    // Delete copy operations - cannot safely copy FILE* and z_stream
    GzipStreamReader(const GzipStreamReader&) = delete;
    GzipStreamReader& operator=(const GzipStreamReader&) = delete;
    
    // Move constructor
    GzipStreamReader(GzipStreamReader&& other) = delete;
    // Move assignment
    GzipStreamReader& operator=(GzipStreamReader&& other) = delete; 
};

//
 // Initialize the stream reader at a specific checkpoint.
 // This replicates what deflate_index_extract does, but keeps the z_stream alive.
 ///
std::unique_ptr<GzipStreamReader> open_gzip_at_checkpoint(
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
    
    // Get the checkpoint
    // struct point* points = static_cast<struct point*>(index->list);
    struct point* checkpoint = &index->list->at(checkpoint_index);
    
    reader->uncompressed_offset = checkpoint->out;
    reader->bits_from_last_byte = checkpoint->bits;
    
    // Seek to the compressed position
    if (fseeko(gz_file, checkpoint->in - (checkpoint->bits ? 1 : 0), SEEK_SET) != 0) {
        throw std::runtime_error("Failed to seek in gzip file");
    }
    reader->file_offset = checkpoint->in - (checkpoint->bits ? 1 : 0);
    
   // When starting from a checkpoint (not at the beginning), we must use raw deflate
    // because there's no gzip header at the checkpoint position
    int windowBits = -15;
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

//
 // Skip gzip header and trailer to get to the next gzip member.
 // Returns 0 on success, -1 on error/EOF, 1 if EOF marker found (for BGZF).
 //
static int skip_gzip_header(z_stream* strm, FILE* file, unsigned char* input_buffer, size_t buffer_size, off_t* file_offset) {
    // After Z_STREAM_END, we need to skip:
    // 1. The 8-byte gzip trailer (CRC32 + ISIZE)
    // 2. The next gzip header (10+ bytes)
    
    //fprintf(stderr, "skip_gzip_header: avail_in=%u, file_offset=%lld\n", 
            //strm->avail_in, (long long)*file_offset);
    
    // The trailer might already be in strm->next_in
    // We need to consume it and then read/skip the next header
    
    // First, discard the trailer (8 bytes) if present in current buffer
    if (strm->avail_in >= 8) {
        strm->next_in += 8;
        strm->avail_in -= 8;
        //fprintf(stderr, "  Skipped trailer from buffer, avail_in now=%u\n", strm->avail_in);
    } else {
        // Need to read more to skip trailer
        size_t to_skip = 8 - strm->avail_in;
        strm->avail_in = 0;
        
        // Read and discard bytes
        unsigned char discard[8];
        size_t actually_read = fread(discard, 1, to_skip, file);
        if (actually_read != to_skip) {
            //fprintf(stderr, "  EOF while reading trailer (needed %zu, got %zu)\n", to_skip, actually_read);
            return -1;  // EOF or error
        }
        *file_offset += actually_read;
        //fprintf(stderr, "  Read and discarded %zu trailer bytes from file\n", actually_read);
    }
    
    // Now skip the gzip header
    // Minimum gzip header is 10 bytes, but can be longer with optional fields
    
    // Read header into buffer if needed
    if (strm->avail_in < 10) {
        //fprintf(stderr, "  Need more data for header (have %u, need 10)\n", strm->avail_in);
        
        // Save any existing data by moving it to the start of the buffer
        if (strm->avail_in > 0 && strm->next_in != input_buffer) {
            memmove(input_buffer, strm->next_in, strm->avail_in);
            //fprintf(stderr, "  Moved %u bytes to start of buffer\n", strm->avail_in);
        }
        
        // Try to read more data
        size_t bytes_read = fread(input_buffer + strm->avail_in, 1, 
                                  buffer_size - strm->avail_in, file);
        //fprintf(stderr, "  Read %zu more bytes from file\n", bytes_read);
        
        if (bytes_read == 0 && strm->avail_in < 10) {
            //fprintf(stderr, "  Not enough data for header\n");
            return -1;  // No more data
        }
        
        *file_offset += bytes_read;
        strm->avail_in += bytes_read;
        strm->next_in = input_buffer;
    }
    
    // Parse gzip header
    if (strm->avail_in < 10) {
        //fprintf(stderr, "  Still not enough data for header (%u bytes)\n", strm->avail_in);
        return -1;  // Not enough data for header
    }
    
    unsigned char* header = strm->next_in;
    
    //fprintf(stderr, "  Header bytes: %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x\n",
     //       header[0], header[1], header[2], header[3], header[4],
      //      header[5], header[6], header[7], header[8], header[9]);
    
    // Check magic number
    if (header[0] != 0x1f || header[1] != 0x8b) {
        fprintf(stderr, "  Bad magic number: %02x %02x\n", header[0], header[1]);
        return -1;  // Not a gzip header
    }
    
    // Check compression method (should be 8 for DEFLATE)
    if (header[2] != 8) {
        fprintf(stderr, "  Bad compression method: %02x\n", header[2]);
        return -1;  // Unsupported compression
    }
    
    unsigned char flags = header[3];
    //fprintf(stderr, "  Flags: %02x\n", flags);
    
    // Skip fixed header
    strm->next_in += 10;
    strm->avail_in -= 10;
    
    // Skip optional fields based on flags
    // FEXTRA (used by BGZF for block size metadata)
    if (flags & 0x04) {
        //fprintf(stderr, "  Processing FEXTRA\n");
        
        // Ensure we have at least 2 bytes for length
        if (strm->avail_in < 2) {
            // Move remaining data to start of buffer
            if (strm->avail_in > 0 && strm->next_in != input_buffer) {
                memmove(input_buffer, strm->next_in, strm->avail_in);
            }
            
            // Read more data
            size_t bytes_read = fread(input_buffer + strm->avail_in, 1, 
                                      buffer_size - strm->avail_in, file);
            *file_offset += bytes_read;
            strm->avail_in += bytes_read;
            strm->next_in = input_buffer;
            
            if (strm->avail_in < 2) {
                fprintf(stderr, "  Not enough data for FEXTRA length\n");
                return -1;
            }
        }
        
        size_t extra_len = strm->next_in[0] | (strm->next_in[1] << 8);
        //fprintf(stderr, "  FEXTRA length: %zu\n", extra_len);
        strm->next_in += 2;
        strm->avail_in -= 2;
        
        // Ensure we have enough bytes for the extra data
        if (strm->avail_in < extra_len) {
            // Move remaining data to start of buffer
            if (strm->avail_in > 0 && strm->next_in != input_buffer) {
                memmove(input_buffer, strm->next_in, strm->avail_in);
            }
            
            // Read more data
            size_t bytes_read = fread(input_buffer + strm->avail_in, 1, 
                                      buffer_size - strm->avail_in, file);
            *file_offset += bytes_read;
            strm->avail_in += bytes_read;
            strm->next_in = input_buffer;
            
            if (strm->avail_in < extra_len) {
                //fprintf(stderr, "  Not enough data for FEXTRA content (need %zu, have %u)\n", 
                // extra_len, strm->avail_in);
                return -1;
            }
        }
        
        strm->next_in += extra_len;
        strm->avail_in -= extra_len;
    }
    
    // FNAME - skip null-terminated filename (rarely used in BGZF)
    if (flags & 0x08) {
        //fprintf(stderr, "  Processing FNAME\n");
        while (strm->avail_in > 0 && *strm->next_in != 0) {
            strm->next_in++;
            strm->avail_in--;
        }
        if (strm->avail_in == 0) {
            size_t bytes_read = fread(input_buffer, 1, buffer_size, file);
            *file_offset += bytes_read;
            if (bytes_read == 0) return -1;
            strm->avail_in = bytes_read;
            strm->next_in = input_buffer;
        }
        strm->next_in++;  // Skip the null terminator
        strm->avail_in--;
    }
    
    // FCOMMENT - skip null-terminated comment (rarely used in BGZF)
    if (flags & 0x10) {
        //fprintf(stderr, "  Processing FCOMMENT\n");
        while (strm->avail_in > 0 && *strm->next_in != 0) {
            strm->next_in++;
            strm->avail_in--;
        }
        if (strm->avail_in == 0) {
            size_t bytes_read = fread(input_buffer, 1, buffer_size, file);
            *file_offset += bytes_read;
            if (bytes_read == 0) return -1;
            strm->avail_in = bytes_read;
            strm->next_in = input_buffer;
        }
        strm->next_in++;  // Skip the null terminator
        strm->avail_in--;
    }
    
    // FHCRC - skip 2-byte header CRC
    if (flags & 0x02) {
        //fprintf(stderr, "  Processing FHCRC\n");
        if (strm->avail_in < 2) {
            // Move remaining data to start of buffer
            if (strm->avail_in > 0 && strm->next_in != input_buffer) {
                memmove(input_buffer, strm->next_in, strm->avail_in);
            }
            
            size_t bytes_read = fread(input_buffer + strm->avail_in, 1, 
                                      buffer_size - strm->avail_in, file);
            *file_offset += bytes_read;
            strm->avail_in += bytes_read;
            strm->next_in = input_buffer;
            
            if (strm->avail_in < 2) return -1;
        }
        strm->next_in += 2;
        strm->avail_in -= 2;
    }
    
    //fprintf(stderr, "  Header skipped successfully, avail_in=%u, ready for deflate data\n", strm->avail_in);
    
    return 0;  // Success
}




//
// Read decompressed data from the stream.
// Returns number of bytes read (0 = EOF, negative = error)
//
ptrdiff_t gzip_read(GzipStreamReader* reader, char* buffer, size_t len) {
    if (!reader || !reader->file) {
        return -1;
    }
    
    z_stream* strm = reader->zstream.get();
    strm->avail_out = len;
    strm->next_out = reinterpret_cast<unsigned char*>(buffer);
    
    while (strm->avail_out > 0) {
        // If we need more input data
        if (strm->avail_in == 0) {
            // Read more compressed data from file
            size_t bytes_read = fread(reader->input_buffer, 1, sizeof(reader->input_buffer), reader->file);
            
            if (bytes_read == 0) {
                if (feof(reader->file)) {
                    // End of compressed data
                    break;
                } else {
                    fprintf(stderr, "File read error\n");
                    return -1;
                }
            }
            
            reader->file_offset += bytes_read;
            strm->avail_in = bytes_read;
            strm->next_in = reader->input_buffer;
        }
        
        // Decompress
        int ret = inflate(strm, Z_NO_FLUSH);
        
        if (ret == Z_STREAM_END) {
            // Reached end of a gzip member (stream)
            // For multi-stream gzip files, we need to reset and continue
            
            // Check if there's more data to read
            if (strm->avail_in > 0 || !feof(reader->file)) {
                // Skip the gzip trailer and next header
                if (skip_gzip_header(strm, reader->file, reader->input_buffer, sizeof(reader->input_buffer), &reader->file_offset) !=0 ) { 
                    //skip_gzip_header(strm, reader->file, reader->input_buffer, sizeof(reader->input_buffer)) != 0) {
                    // No more gzip members, we're done
                    break;
                }
                
                // Reset inflate to start processing the next member's deflate stream
                // Use inflateReset (not inflateReset2) to keep raw deflate mode
                ret = inflateReset(strm);
                if (ret != Z_OK) {
                    fprintf(stderr, "inflateReset() failed: %d\n", ret);
                    return -1;
                }
                
                // Continue decompressing from the next member
                continue;
            } else {
                // Truly at the end of the file
                break;
            }
        } else if (ret == Z_OK) {
            // Successfully decompressed some data, continue
            continue;
        } else if (ret == Z_BUF_ERROR) {
            // No progress possible (need more input or output space)
            // This shouldn't happen with our logic, but continue anyway
            continue;
        } else {
            // Error occurred
            fprintf(stderr, "inflate() error: %d (%s)\n", ret, 
                    strm->msg ? strm->msg : "no message");
            if (ret == Z_NEED_DICT) {
                fprintf(stderr, "Z_NEED_DICT - dictionary not set properly\n");
            } else if (ret == Z_DATA_ERROR) {
                fprintf(stderr, "Z_DATA_ERROR - corrupted data or wrong position\n");
            } else if (ret == Z_MEM_ERROR) {
                fprintf(stderr, "Z_MEM_ERROR - out of memory\n");
            }
            return -1;
        }
    }
    
    size_t bytes_produced = len - strm->avail_out;
    reader->uncompressed_offset += bytes_produced;
    
    return bytes_produced;
}
*/


#endif //ZRAN_INDEX_HELPERS_HPP

