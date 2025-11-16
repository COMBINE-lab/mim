#ifndef ZRAN_INDEX_HELPERS_HPP
#define ZRAN_INDEX_HELPERS_HPP

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <zlib.h>
//#include "zran.hpp"

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
    GzipStreamReader(GzipStreamReader&& other) = delete;
    // Move assignment
    GzipStreamReader& operator=(GzipStreamReader&& other) = delete; 
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
    // struct point* points = static_cast<struct point*>(index->list);
    struct point* checkpoint = &index->list->at(checkpoint_index);
    
    reader->uncompressed_offset = checkpoint->out;
    reader->bits_from_last_byte = checkpoint->bits;
    
    // Seek to the compressed position
    if (fseeko(gz_file, checkpoint->in - (checkpoint->bits ? 1 : 0), SEEK_SET) != 0) {
        throw std::runtime_error("Failed to seek in gzip file");
    }
    reader->file_offset = checkpoint->in - (checkpoint->bits ? 1 : 0);
    
    // Initialize the inflate stream
    // Use raw deflate mode (negative window bits)
    //int windowBits = index->gzip ? 47 : -15;  // 47 = 32 + 15 for gzip, -15 for raw deflate
    int windowBits = RAW;  
    reader->zstream.init(windowBits);
  
    z_stream* strm = reader->zstream.get();

    // Set the decompression dictionary (the 32KB window from the checkpoint)
    if (checkpoint->out > 0) {
        //(&index->strm, curr_point->window, curr_point->dict);
        reader->zstream.setDictionary(checkpoint->window, checkpoint->dict);
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
 * Read decompressed data from the stream.
 * Returns number of bytes read (0 = EOF, negative = error)
 */
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
            // Finished decompressing this stream
            break;
        } else if (ret == Z_OK) {
            // Successfully decompressed some data, continue
            continue;
        } else if (ret == Z_BUF_ERROR) {
            // No progress possible (need more input or output space)
            // This shouldn't happen with our logic, but continue anyway
            continue;
        } else {
            // Error occurred
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

#endif //ZRAN_INDEX_HELPERS_HPP

