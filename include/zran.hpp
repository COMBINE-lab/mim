/*
 * This file is originally from
 * https://github.com/madler/zlib/blob/master/examples/zran.c and is adapted for
 * our usecase for parallel decompression of gzipped FASTQ files.
 *
 * License:
 * Copyright notice:
 *
 * (C) 1995-2024 Jean-loup Gailly and Mark Adler
 *
 * This software is provided 'as-is', without any express or implied
 * warranty.  In no event will the authors be held liable for any damages
 * arising from the use of this software.
 *
 * Permission is granted to anyone to use this software for any purpose,
 * including commercial applications, and to alter it and redistribute it
 * freely, subject to the following restrictions:
 *
 * 1. The origin of this software must not be misrepresented; you must not
 *   claim that you wrote the original software. If you use this software
 *   in a product, an acknowledgment in the product documentation would be
 *   appreciated but is not required.
 * 2. Altered source versions must be plainly marked as such, and must not be
 *   misrepresented as being the original software.
 * 3. This notice may not be removed or altered from any source distribution.
 *
 * Jean-loup Gailly        Mark Adler
 * jloup@gzip.org          madler@alumni.caltech.edu
 *
 *
 * Modified by:
 * Siddhant Bharti <sbharti@umd.edu>
 * Prajwal Singhania <prajwal@umd.edu>,
 * Rakrish Dhakal <rakrish@umd.edu>,
 * Rob Patro <rob@cs.umd.edu>
 */

#ifndef ZRAN_HPP
#define ZRAN_HPP
#include "kseq++/seqio.hpp"
#include <chrono>
#include <climits>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <iostream>
#include <limits>
#include <sstream>
#include <stdexcept>
#include <utility>
#include <optional>
#include "zlib.h"
#include "json.hpp"
#include "blake3.h"

using namespace std;

constexpr char MIMINDEX_STR[] = "MIMINDEX";
constexpr int MIMINDEX_LEN = sizeof(MIMINDEX_STR) - 1; // 8

#define WINSIZE 32768U // sliding window size
#define CHUNK 16384    // file input buffer size

// Decompression modes. These are the inflateInit2() windowBits parameter.
#define RAW -15
#define ZLIB 15
#define GZIP 31

#define SPAN 1048576L // desired distance between access points
#define LEN 10        // number of bytes to extract

// Access point.
typedef struct point {
  off_t out;             // offset in uncompressed data
  off_t in;              // offset in compressed file of first full byte
  int bits;              // 0, or number of bits (1-7) from byte at in-1
  unsigned dict;         // number of bytes in window to use as a dictionary
  unsigned char *window; // preceding 32K (or less) of uncompressed data
} point_t;

// Information necessary for getting the first record
// after an access point.
struct record_checkpoint {
  uint64_t first_record_in_chunk; // the record number of the first record in this chunk
  uint64_t byte_offset; // byte offset of the record in the chunk
};

// Access point list.
struct deflate_index {
  std::vector<std::uint8_t>* metadata_dict; // CBOR blob
  int have;              // number of access points in list
  int mode;              // -15 for raw, 15 for zlib, or 31 for gzip
  off_t length;          // total length of uncompressed data
  std::vector<point_t>* list; // allocated list of access points
  //point_t *list;        /* got rid of this in favor of vector above */ 
  z_stream strm;         // re-usable inflate engine for extraction
  int record_chunk_size; // the number of records in each chunk
  vector<record_checkpoint>
      *record_boundaries;  // stores bytes offsets of records in FASTQ file
  off_t num_record_chunks; // number of records in FASTQ file
  off_t total_record_count;
  uint8_t compressed_hash[BLAKE3_OUT_LEN];

  deflate_index() {
    have = -1;
    mode = -1;
    length = 0;
    list = nullptr;
    num_record_chunks = 0;
    total_record_count = 0;
    memset(compressed_hash, 0, BLAKE3_OUT_LEN);
  }

  // Copy constructor - Shallow copy
  deflate_index(deflate_index &other) {
    have = other.have;
    mode = other.mode;
    length = other.length;
    list = other.list;
    inflateCopy(&strm, &other.strm);
    record_boundaries = other.record_boundaries;
    num_record_chunks = other.num_record_chunks;
    total_record_count = other.total_record_count;
    memcpy(compressed_hash, other.compressed_hash, BLAKE3_OUT_LEN);
  }
};

inline void print_point(point_t &point) {
  // Print an entire access point in one line
  std::cout << "out: " << point.out << ", in: " << point.in
            << ", bits: " << point.bits << ", dict: " << point.dict
            << ", window: ";
  for (int i = 0; i < 10; i++) {
    std::cout << (int)point.window[i] << " ";
  }
  std::cout << std::endl;
}

inline void print_index(struct deflate_index *index) {
  std::cout << "================ index: ================" << std::endl;

  // Print metadata
  std::cout << "mode: " << index->mode << std::endl;
  std::cout << "length: " << index->length << std::endl;
  std::cout << "have: " << index->have << std::endl;

  // Print strm
  std::cout << "strm.avail_in: " << index->strm.avail_in << std::endl;
  std::cout << "strm.avail_out: " << index->strm.avail_out << std::endl;
  std::cout << "strm.data_type: " << index->strm.data_type << std::endl;
  std::cout << "strm.zalloc: " << index->strm.zalloc << std::endl;
  std::cout << "strm.zfree: " << index->strm.zfree << std::endl;
  std::cout << "strm.opaque: " << index->strm.opaque << std::endl;

  std::cout << "record chunk size: " << index->record_chunk_size << std::endl;
  // Print access points
  for (int i = 0; i < index->have; i++) {
    print_point((*index->list)[i]);
  }
  std::cout << "========================================" << std::endl;
}

inline void deflate_index_free(struct deflate_index *index) {
  fprintf(stderr, "zran: freeing index\n");
  if (index != nullptr) {
    size_t i = index->have;
    while (i)
      free((*index->list)[--i].window);
    //free(index->list);
    inflateEnd(&index->strm);
    free(index);
  }
}

template <typename T>
inline int write_vector_to_gzfile(gzFile out, std::vector<T>& v, const std::string desc) {
  constexpr size_t max_buf_write = std::numeric_limits<int>::max();
  // the metadata
  size_t vec_len = v.size();
  auto elem_t_size = sizeof(decltype(v.front()));
  if ((elem_t_size * vec_len) >= max_buf_write) {
    fprintf(stderr, "%s too large to write in gzwrite() call\n", desc.c_str());
    return Z_ERRNO;
  }
  if (gzwrite(out, &vec_len, sizeof(vec_len)) != sizeof(vec_len) ||
      gzwrite(out, v.data(), elem_t_size * vec_len) != static_cast<int>(elem_t_size * vec_len)) {
    return Z_ERRNO;
  }
  return 0;
}

// Save index to gzip file.
inline int deflate_index_save_gzip(gzFile out, struct deflate_index *index) {
  constexpr size_t max_buf_write = std::numeric_limits<int>::max();

  // first write the magic header
  if (gzwrite(out, MIMINDEX_STR, MIMINDEX_LEN) != MIMINDEX_LEN) {
    fprintf(stderr, "Could not write magic header out to file!");
  }

  long long int index_size = 0;
  long long int offset_size = 0;
  // Write metadata
  // for the metadata dictionary
  // index_size += index->metadata_dict.size();

  index_size += sizeof(index->mode);
  index_size += sizeof(index->have);
  index_size += sizeof(index->length);
  index_size += sizeof(index->num_record_chunks);
  index_size += BLAKE3_OUT_LEN;

  auto start = std::chrono::high_resolution_clock::now();
  // the metadata
  auto ret = write_vector_to_gzfile<uint8_t>(out, *index->metadata_dict, "metadata_dict");
  if (ret != 0) { return ret; }

  if (gzwrite(out, &index->mode, sizeof(index->mode)) != sizeof(index->mode) ||
      gzwrite(out, &index->have, sizeof(index->have)) != sizeof(index->have) ||
      gzwrite(out, &index->length, sizeof(index->length)) !=
          sizeof(index->length) ||
      gzwrite(out, &index->num_record_chunks, sizeof(index->num_record_chunks)) != sizeof(index->num_record_chunks) ||
      gzwrite(out, &index->compressed_hash, BLAKE3_OUT_LEN) != BLAKE3_OUT_LEN) {
    return Z_ERRNO;
  }

  // Write access points
  for (int i = 0; i < index->have; i++) {
    point_t& point = (*index->list)[i];
    index_size += sizeof(point.out);
    index_size += sizeof(point.in);
    index_size += sizeof(point.bits);
    index_size += point.dict;
    if (gzwrite(out, &point.out, sizeof(point.out)) != sizeof(point.out) ||
        gzwrite(out, &point.in, sizeof(point.in)) != sizeof(point.in) ||
        gzwrite(out, &point.bits, sizeof(point.bits)) !=
            sizeof(point.bits) ||
        gzwrite(out, &point.dict, sizeof(point.dict)) !=
            sizeof(point.dict) ||
        gzwrite(out, point.window, point.dict) != static_cast<int>(point.dict)) {
      return Z_ERRNO;
    }
  }

  // Write record boundaries
  size_t boundaries_count = index->record_boundaries->size();
  using elem_t =
      typename std::decay<decltype(*index->record_boundaries->begin())>::type;
  auto elem_t_size = sizeof(elem_t);
  offset_size += sizeof(boundaries_count);
  offset_size += elem_t_size * boundaries_count;
  ret = write_vector_to_gzfile<elem_t>(out, *index->record_boundaries, "boundaries vector");
  if (ret != 0) { return ret; }
  
  index_size += sizeof(index->total_record_count);
  if (gzwrite(out, &index->total_record_count, sizeof(index->total_record_count)) != sizeof(index->total_record_count)) {
    return Z_ERRNO;
  }

  gzclose(out);
  auto end = std::chrono::high_resolution_clock::now();
  auto duration =
      std::chrono::duration_cast<std::chrono::milliseconds>(end - start);
  cout << "index_size " << index_size << " "
       << (index_size * 1.0) / (index_size * 1.0 + offset_size * 1.0) << endl;
  cout << "offset_size " << offset_size << " "
       << (offset_size * 1.0) / (index_size * 1.0 + offset_size * 1.0) << endl;
  cout << "Time to save the gzip index " << duration.count() << " milliseconds"
       << endl;
  return 0;
}

// Read index from gzip file.
inline int deflate_index_load_gzip(gzFile in, struct deflate_index **built) {
  auto start = std::chrono::high_resolution_clock::now();
  constexpr size_t max_buf_read = std::numeric_limits<int>::max();

  // first read the magic header
  char buf[MIMINDEX_LEN];

  // Attempt to read exactly 8 bytes.
  int n = gzread(in, buf, MIMINDEX_LEN);
  if (n != MIMINDEX_LEN) {
    fprintf(stderr, "gzread failed to read magic header!\n");
    return Z_ERRNO;
  }

  // Compare with the expected string.
  if (std::memcmp(buf, MIMINDEX_STR, MIMINDEX_LEN) != 0) {
        fprintf(stderr, "Invalid header: expected MIMINDEX\n");
      return -1;
  }
 
  struct deflate_index *index =
      (struct deflate_index *)malloc(sizeof(struct deflate_index));
  if (index == nullptr)
    return Z_MEM_ERROR;

  // read metadata_dict
  size_t metadata_dict_len = 0;
  using metadata_dict_elem_t = typename std::decay<decltype(*index->metadata_dict->begin())>::type;
  size_t metadata_dict_elem_t_size = sizeof(metadata_dict_elem_t);
  if (gzread(in, &metadata_dict_len, sizeof(metadata_dict_len)) != sizeof(metadata_dict_len)) {
    return Z_ERRNO;
  }
  index->metadata_dict = new std::vector<metadata_dict_elem_t>();
  index->metadata_dict->resize(metadata_dict_len);
  if ((metadata_dict_elem_t_size * metadata_dict_len) >= max_buf_read) {
    fprintf(stderr, "metadat_dict is too large for gzread.");
    return Z_ERRNO;
  }
  if (gzread(in, index->metadata_dict->data(), metadata_dict_elem_t_size * metadata_dict_len) != static_cast<int>(metadata_dict_elem_t_size * metadata_dict_len)) {
    deflate_index_free(index);
    return Z_ERRNO;
  }
  
  // Read metadata
  if (gzread(in, &index->mode, sizeof(index->mode)) != sizeof(index->mode) ||
      gzread(in, &index->have, sizeof(index->have)) != sizeof(index->have) ||
      gzread(in, &index->length, sizeof(index->length)) !=
          sizeof(index->length) ||
      gzread(in, &index->num_record_chunks, sizeof(index->num_record_chunks)) !=
          sizeof(index->num_record_chunks) ||
      gzread(in, &index->compressed_hash, BLAKE3_OUT_LEN) != BLAKE3_OUT_LEN) {
    return Z_ERRNO;
  }

  // Read access points
  index->list = new std::vector<point_t>(index->have);
  //index->list->resize(index->have);//= (point_t *)malloc(sizeof(point_t) * index->have);
  /*
  if (index->list == nullptr) {
    deflate_index_free(index);
    return Z_MEM_ERROR;
  }
  */

  for (int i = 0; i < index->have; i++) {
    point_t& point = (*index->list)[i];

    if (gzread(in, &point.out, sizeof(point.out)) != sizeof(point.out) ||
        gzread(in, &point.in, sizeof(point.in)) != sizeof(point.in) ||
        gzread(in, &point.bits, sizeof(point.bits)) != sizeof(point.bits) ||
        gzread(in, &point.dict, sizeof(point.dict)) != sizeof(point.dict)) {
      //deflate_index_free(index);
      return Z_ERRNO;
    }

    point.window = (unsigned char *)malloc(point.dict);
    if (point.window == nullptr) {
      deflate_index_free(index);
      return Z_MEM_ERROR;
    }
    if (gzread(in, point.window, point.dict) != static_cast<int>(point.dict)) {
      deflate_index_free(index);
      return Z_ERRNO;
    }
  }
  // Read record boundaries
  size_t boundaries_count;
  if (gzread(in, &boundaries_count, sizeof(boundaries_count)) !=
      sizeof(boundaries_count)) {
    deflate_index_free(index);
    return Z_ERRNO;
  }

  using elem_t =
      typename std::decay<decltype(*index->record_boundaries->begin())>::type;
  auto elem_t_size = sizeof(elem_t);
  index->record_boundaries = new std::vector<elem_t>();
  index->record_boundaries->resize(boundaries_count);
  if ((elem_t_size * boundaries_count) >= max_buf_read) {
    fprintf(stderr, "record_boundaries is too large for gzread.");
    return Z_ERRNO;
  }
  if (gzread(in, index->record_boundaries->data(), elem_t_size * boundaries_count) != static_cast<int>(elem_t_size * boundaries_count)) {
    deflate_index_free(index);
    return Z_ERRNO;
  }

  off_t total_record_count{0};
  if (gzread(in, &total_record_count, sizeof(total_record_count)) != sizeof(total_record_count)) {
    return Z_ERRNO;
  }
  index->total_record_count = total_record_count;
  gzclose(in);

  // Initialize inflation state
  index->strm.zalloc = Z_NULL;
  index->strm.zfree = Z_NULL;
  index->strm.opaque = Z_NULL;
  inflateInit2(&index->strm, index->mode);

  // Return index
  *built = index;
  auto end = std::chrono::high_resolution_clock::now();
  auto duration =
      std::chrono::duration_cast<std::chrono::milliseconds>(end - start);
  cerr << "Time to load the gzip index " << duration.count() << " milliseconds"
       << endl;
  cerr << "total record count: " << index->total_record_count << "\n";
  return index->have;
}

// Add an access point to the list. If out of memory, deallocate the existing
// list and return nullptr. index->mode is temporarily the allocated number of
// access points, until it is time for deflate_index_build() to return. Then
// index->mode is set to the mode of inflation.
static struct deflate_index *add_point(struct deflate_index *index, off_t in,
                                       off_t out, off_t beg,
                                       unsigned char *window) {
  if (index->have == index->mode) {
    // The list is full. Make it bigger.
    index->mode = index->mode ? index->mode << 1 : 8;
    index->list->resize(index->mode);
    fprintf(stderr, "mode: %d, have: %ld\n", index->mode, index->have);
    /*
    point_t *next =
        (point_t *)realloc(index->list, sizeof(point_t) * index->mode);
    if (next == nullptr) {
      deflate_index_free(index);
      return nullptr;
    }
    index->list = next;
    */
  }

  // Fill in the access point and increment how many we have.
  point_t &next = (*index->list)[index->have++];//(point_t *)(index->list) + index->have++;
  if (index->have < 0) {
    fprintf(stderr, "no, sparky; bad boy!\n");
    // Overflowed the int!
    //deflate_index_free(index);
    return nullptr;
  }
  next.out = out;
  next.in = in;
  next.bits = index->strm.data_type & 7;
  next.dict = out - beg > WINSIZE ? WINSIZE : (unsigned)(out - beg);
  next.window = (unsigned char *)malloc(next.dict);
  if (next.window == nullptr) {
    fprintf(stderr, "zran::add_checkpoint:: next.window == nullptr!\n");
    //deflate_index_free(index);
    return nullptr;
  }
  unsigned recent = WINSIZE - index->strm.avail_out;
  unsigned copy = recent > next.dict ? next.dict : recent;
  memcpy(next.window + next.dict - copy, window + recent - copy, copy);
  copy = next.dict - copy;
  memcpy(next.window, window + WINSIZE - copy, copy);

  // Return the index, which may have been newly allocated or destroyed.
  return index;
}

inline int deflate_index_build(FILE *in, off_t span,
                               struct deflate_index **built) {
  blake3_hasher hasher;
  blake3_hasher_init(&hasher);
  // If this returns with an error, any attempt to use the index will cleanly
  // return an error.
  *built = nullptr;

  // Create and initialize the index list.
  struct deflate_index *index =
      (struct deflate_index *)malloc(sizeof(struct deflate_index));
  if (index == nullptr)
    return Z_MEM_ERROR;
  index->have = 0;
  index->mode = 0; // entries in index->list allocation
  index->list = new std::vector<point_t>();
  index->strm.state = Z_NULL; // so inflateEnd() can work

  // Set up the inflation state.
  index->strm.avail_in = 0;
  index->strm.avail_out = 0;
  unsigned char buf[CHUNK];         // input buffer
  unsigned char win[WINSIZE] = {0}; // output sliding window
  off_t totin = 0;                  // total bytes read from input
  off_t totout = 0;                 // total bytes uncompressed
  off_t beg = 0;                    // starting offset of last history reset
  int mode = 0; // mode: RAW, ZLIB, or GZIP (0 => not set yet)

  // Decompress from in, generating access points along the way.
  int ret;    // the return value from zlib, or Z_ERRNO
  off_t last = 0; // last access point uncompressed offset
  do {
    // Assure available input, at least until reaching EOF.
    if (index->strm.avail_in == 0) {
      index->strm.avail_in = fread(buf, 1, sizeof(buf), in);
      totin += index->strm.avail_in;
      index->strm.next_in = buf;
      if (index->strm.avail_in < sizeof(buf) && ferror(in)) {
        ret = Z_ERRNO;
        break;
      }
      blake3_hasher_update(&hasher, index->strm.next_in, index->strm.avail_in);

      if (mode == 0) {
        // At the start of the input -- determine the type. Assume raw
        // if it is neither zlib nor gzip. This could in theory result
        // in a false positive for zlib, but in practice the fill bits
        // after a stored block are always zeros, so a raw stream won't
        // start with an 8 in the low nybble.
        mode = index->strm.avail_in == 0 ? RAW : // will fail
                   (index->strm.next_in[0] & 0xf) == 8 ? ZLIB
               : index->strm.next_in[0] == 0x1f        ? GZIP
                                                       :
                                                /* else */ RAW;
        index->strm.zalloc = Z_NULL;
        index->strm.zfree = Z_NULL;
        index->strm.opaque = Z_NULL;
        ret = inflateInit2(&index->strm, mode);
        if (ret != Z_OK)
          break;
      }
    }

    // Assure available output. This rotates the output through, for use as
    // a sliding window on the uncompressed data.
    if (index->strm.avail_out == 0) {
      index->strm.avail_out = sizeof(win);
      index->strm.next_out = win;
    }

    if (mode == RAW && index->have == 0)
      // We skip the inflate() call at the start of raw deflate data in
      // order generate an access point there. Set data_type to imitate
      // the end of a header.
      index->strm.data_type = 0x80;
    else {
      // Inflate and update the number of uncompressed bytes.
      unsigned before = index->strm.avail_out;
      ret = inflate(&index->strm, Z_BLOCK);
      totout += before - index->strm.avail_out;
    }

    //bool added_access_point = false;
    if ((index->strm.data_type & 0xc0) == 0x80 &&
        (index->have == 0 || totout - last >= span)) {
      // We are at the end of a header or a non-last deflate block, so we
      // can add an access point here. Furthermore, we are either at the
      // very start for the first access point, or there has been span or
      // more uncompressed bytes since the last access point, so we want
      // to add an access point here.
      index = add_point(index, totin - index->strm.avail_in, totout, beg, win);
      if (index->have % 100 == 0) {
        fprintf(stderr, "adding access point %ld at %ld (read) %ld (written); distance %ld vs span %ld.\n", index->have, (totin - index->strm.avail_in), totout, totout - last, span);
      }
      if (index == nullptr) {
        ret = Z_MEM_ERROR;
        break;
      }
      last = totout;
      //added_access_point = true;
    }

    if (ret == Z_STREAM_END && mode == GZIP &&
        (index->strm.avail_in || ungetc(getc(in), in) != EOF)) {
      // There is more input after the end of a gzip member. Reset the
      // inflate state to read another gzip member. On success, this will
      // set ret to Z_OK to continue decompressing.
      ret = inflateReset2(&index->strm, GZIP);
      //if (added_access_point) { 
        beg = totout; // reset history
      // }
    }

    // Keep going until Z_STREAM_END or error. If the compressed data ends
    // prematurely without a file read error, Z_BUF_ERROR is returned.
  } while (ret == Z_OK);

  if (ret != Z_STREAM_END) {
    // An error was encountered. Discard the index and return a negative
    // error code.
    deflate_index_free(index);
    return ret == Z_NEED_DICT ? Z_DATA_ERROR : ret;
  }

  uint8_t output[BLAKE3_OUT_LEN];
  blake3_hasher_finalize(&hasher, output, BLAKE3_OUT_LEN);
  std::cout << "BLAKE 3 checkum \n";
    // Print the hash as hexadecimal.
  for (size_t i = 0; i < BLAKE3_OUT_LEN; i++) {
    printf("%02x", output[i]);
  }
  printf("\n");
  // Return the index.
  index->mode = mode;
  index->length = totout;
  std::memcpy(index->compressed_hash, output, BLAKE3_OUT_LEN);
  *built = index;
  return index->have;
}

#define INFLATEPRIME inflatePrime

inline ptrdiff_t deflate_index_extract_with_chunk_index(FILE *in, struct deflate_index *index,
                                       off_t offset, off_t chunk_idx, unsigned char *buf,
                                       size_t len) {
  // Do a quick sanity check on the index.
  if (index == nullptr || index->have < 1 || (*index->list)[0].out != 0) {
    std::cout << "zran: index is not ready" << std::endl;
    return Z_STREAM_ERROR;
  }

  // If nothing to extract, return zero bytes extracted.
  if (len == 0 || offset < 0 || offset >= index->length) {
    std::cout << "zran: nothing to extract" << std::endl;
    return 0;
  }
  auto curr_point = (index->list->begin() + chunk_idx);

  // Initialize the input file and prime the inflate engine to start there.
  int ret = fseeko(in, curr_point->in - (curr_point->bits ? 1 : 0), SEEK_SET);
  if (ret == -1) {
    std::cout << "zran: seek error" << std::endl;
    return Z_ERRNO;
  }
  int ch = 0;
  if (curr_point->bits && (ch = getc(in)) == EOF)
    return ferror(in) ? Z_ERRNO : Z_BUF_ERROR;
  index->strm.avail_in = 0;
  ret = inflateReset2(&index->strm, RAW);
  if (ret != Z_OK) {
    std::cout << "zran: inflateReset2 error" << std::endl;
    return ret;
  }
  if (curr_point->bits)
    INFLATEPRIME(&index->strm, curr_point->bits, ch >> (8 - curr_point->bits));
  inflateSetDictionary(&index->strm, curr_point->window, curr_point->dict);

  // Skip uncompressed bytes until offset reached, then satisfy request.
  unsigned char input[CHUNK];
  unsigned char discard[WINSIZE];
  offset -= curr_point->out; // number of bytes to skip to get to offset
  size_t left = len;    // number of bytes left to read after offset
  do {
    if (offset) {
      // Discard up to offset uncompressed bytes.
      index->strm.avail_out = offset < WINSIZE ? (unsigned)offset : WINSIZE;
      index->strm.next_out = discard;
    } else {
      // Uncompress up to left bytes into buf.
      index->strm.avail_out = left < UINT_MAX ? (unsigned)left : UINT_MAX;
      index->strm.next_out = buf + len - left;
    }

    // Uncompress, setting got to the number of bytes uncompressed.
    if (index->strm.avail_in == 0) {
      // Assure available input.
      index->strm.avail_in = fread(input, 1, CHUNK, in);
      if (index->strm.avail_in < CHUNK && ferror(in)) {
        ret = Z_ERRNO;
        break;
      }
      index->strm.next_in = input;
    }
    unsigned got = index->strm.avail_out;
    ret = inflate(&index->strm, Z_NO_FLUSH);
    got -= index->strm.avail_out;

    // Update the appropriate count.
    if (offset)
      offset -= got;
    else {
      left -= got;
      if (left == 0)
        // Request satisfied.
        break;
    }

    // If we're at the end of a gzip member and there's more to read,
    // continue to the next gzip member.
    if (ret == Z_STREAM_END && index->mode == GZIP) {
      // Discard the gzip trailer.
      unsigned drop = 8; // length of gzip trailer
      if (index->strm.avail_in >= drop) {
        index->strm.avail_in -= drop;
        index->strm.next_in += drop;
      } else {
        // Read and discard the remainder of the gzip trailer.
        drop -= index->strm.avail_in;
        index->strm.avail_in = 0;
        do {
          if (getc(in) == EOF) {
            // The input does not have a complete trailer.
            std::cout << "zran: unexpected EOF" << std::endl;
            return ferror(in) ? Z_ERRNO : Z_BUF_ERROR;
          }
        } while (--drop);
      }

      if (index->strm.avail_in || ungetc(getc(in), in) != EOF) {
        // There's more after the gzip trailer. Use inflate to skip the
        // gzip header and resume the raw inflate there.
        inflateReset2(&index->strm, GZIP);
        do {
          if (index->strm.avail_in == 0) {
            index->strm.avail_in = fread(input, 1, CHUNK, in);
            if (index->strm.avail_in < CHUNK && ferror(in)) {
              ret = Z_ERRNO;
              break;
            }
            index->strm.next_in = input;
          }
          index->strm.avail_out = WINSIZE;
          index->strm.next_out = discard;
          ret = inflate(&index->strm, Z_BLOCK); // stop after header
        } while (ret == Z_OK && (index->strm.data_type & 0x80) == 0);
        if (ret != Z_OK)
          break;
        inflateReset2(&index->strm, RAW);
      }
    }

    // Continue until we have the requested data, the deflate data has
    // ended, or an error is encountered.
  } while (ret == Z_OK);

  // Return the number of uncompressed bytes read into buf, or the error.
  // return ret == Z_OK || ret == Z_STREAM_END ? len - left : ret;

  if (ret == Z_OK || ret == Z_STREAM_END) {
    return len - left;
  } else {
    std::cout << "zran: inflate error" << std::endl;
    return ret;
  }
}

inline ptrdiff_t deflate_index_extract(FILE *in, struct deflate_index *index,
                                       off_t offset, unsigned char *buf,
                                       size_t len) {
  // Do a quick sanity check on the index.
  if (index == nullptr || index->have < 1 || (*index->list)[0].out != 0) {
    std::cout << "zran: index is not ready" << std::endl;
    return Z_STREAM_ERROR;
  }

  // If nothing to extract, return zero bytes extracted.
  if (len == 0 || offset < 0 || offset >= index->length) {
    std::cout << "zran: nothing to extract" << std::endl;
    return 0;
  }

  // Find the access point closest to but not after offset.
  int lo = -1, hi = index->have;
  std::vector<point_t>& point = *index->list;//index.list;
  while (hi - lo > 1) {
    int mid = (lo + hi) >> 1;
    if (offset < point[mid].out)
      hi = mid;
    else
      lo = mid;
  }
  auto curr_point = (index->list->begin() + lo);

  // Initialize the input file and prime the inflate engine to start there.
  int ret = fseeko(in, curr_point->in - (curr_point->bits ? 1 : 0), SEEK_SET);
  if (ret == -1) {
    std::cout << "zran: seek error" << std::endl;
    return Z_ERRNO;
  }
  int ch = 0;
  if (curr_point->bits && (ch = getc(in)) == EOF)
    return ferror(in) ? Z_ERRNO : Z_BUF_ERROR;
  index->strm.avail_in = 0;
  ret = inflateReset2(&index->strm, RAW);
  if (ret != Z_OK) {
    std::cout << "zran: inflateReset2 error" << std::endl;
    return ret;
  }
  if (curr_point->bits)
    INFLATEPRIME(&index->strm, curr_point->bits, ch >> (8 - curr_point->bits));
  inflateSetDictionary(&index->strm, curr_point->window, curr_point->dict);

  // Skip uncompressed bytes until offset reached, then satisfy request.
  unsigned char input[CHUNK];
  unsigned char discard[WINSIZE];
  offset -= curr_point->out; // number of bytes to skip to get to offset
  size_t left = len;    // number of bytes left to read after offset
  do {
    if (offset) {
      // Discard up to offset uncompressed bytes.
      index->strm.avail_out = offset < WINSIZE ? (unsigned)offset : WINSIZE;
      index->strm.next_out = discard;
    } else {
      // Uncompress up to left bytes into buf.
      index->strm.avail_out = left < UINT_MAX ? (unsigned)left : UINT_MAX;
      index->strm.next_out = buf + len - left;
    }

    // Uncompress, setting got to the number of bytes uncompressed.
    if (index->strm.avail_in == 0) {
      // Assure available input.
      index->strm.avail_in = fread(input, 1, CHUNK, in);
      if (index->strm.avail_in < CHUNK && ferror(in)) {
        ret = Z_ERRNO;
        break;
      }
      index->strm.next_in = input;
    }
    unsigned got = index->strm.avail_out;
    ret = inflate(&index->strm, Z_NO_FLUSH);
    got -= index->strm.avail_out;

    // Update the appropriate count.
    if (offset)
      offset -= got;
    else {
      left -= got;
      if (left == 0)
        // Request satisfied.
        break;
    }

    // If we're at the end of a gzip member and there's more to read,
    // continue to the next gzip member.
    if (ret == Z_STREAM_END && index->mode == GZIP) {
      // Discard the gzip trailer.
      unsigned drop = 8; // length of gzip trailer
      if (index->strm.avail_in >= drop) {
        index->strm.avail_in -= drop;
        index->strm.next_in += drop;
      } else {
        // Read and discard the remainder of the gzip trailer.
        drop -= index->strm.avail_in;
        index->strm.avail_in = 0;
        do {
          if (getc(in) == EOF) {
            // The input does not have a complete trailer.
            std::cout << "zran: unexpected EOF" << std::endl;
            return ferror(in) ? Z_ERRNO : Z_BUF_ERROR;
          }
        } while (--drop);
      }

      if (index->strm.avail_in || ungetc(getc(in), in) != EOF) {
        // There's more after the gzip trailer. Use inflate to skip the
        // gzip header and resume the raw inflate there.
        inflateReset2(&index->strm, GZIP);
        do {
          if (index->strm.avail_in == 0) {
            index->strm.avail_in = fread(input, 1, CHUNK, in);
            if (index->strm.avail_in < CHUNK && ferror(in)) {
              ret = Z_ERRNO;
              break;
            }
            index->strm.next_in = input;
          }
          index->strm.avail_out = WINSIZE;
          index->strm.next_out = discard;
          ret = inflate(&index->strm, Z_BLOCK); // stop after header
        } while (ret == Z_OK && (index->strm.data_type & 0x80) == 0);
        if (ret != Z_OK)
          break;
        inflateReset2(&index->strm, RAW);
      }
    }

    // Continue until we have the requested data, the deflate data has
    // ended, or an error is encountered.
  } while (ret == Z_OK);

  // Return the number of uncompressed bytes read into buf, or the error.
  // return ret == Z_OK || ret == Z_STREAM_END ? len - left : ret;

  if (ret == Z_OK || ret == Z_STREAM_END) {
    return len - left;
  } else {
    std::cout << "zran: inflate error" << std::endl;
    return ret;
  }
}

inline void build_index(const char *gzFile1, off_t span, nlohmann::json&& user_metadata, std::optional<std::string>& opt_out) {
  // open the input gzipped FASTA/Q file
  FILE *in = fopen(gzFile1, "rb");
  if (in == nullptr) {
    std::stringstream ss;
    ss << "Could not open the given file [" << gzFile1 << "] for reading\n";
    throw runtime_error(ss.str());
    return;
  }
  struct deflate_index *index = nullptr;
  int len = deflate_index_build(in, span, &index);

  if (len < 0) {
    fclose(in);
    switch (len) {
    case Z_MEM_ERROR:
      throw runtime_error("Ran out of memory while building index");
      break;
    case Z_BUF_ERROR:
      throw runtime_error("Building index ended prematurely");
      break;
    case Z_DATA_ERROR:
      throw runtime_error("Saw compressed data error while building index");
      break;
    case Z_ERRNO:
      throw runtime_error("Saw read error while building index");
      break;
    default:
      throw runtime_error("Saw error while building index");
    }
    return;
  }

  fprintf(stderr, "zran: built index with %d access points!\n", len);

  // metadata
  using json = nlohmann::json;
  json j;
  j["version"] = "1.0.0";
  j["user_metadata"] = user_metadata;
  const auto cbor_dat = json::to_cbor(j);
  index->metadata_dict = new vector<uint8_t>(cbor_dat);
  
  //print_index(index);
  fprintf(stderr, "Getting records boundaries from FASTQ file\n");
  klibpp::KSeq record;
  klibpp::SeqStreamIn iss(gzFile1);
  uint64_t RECORD_SPAN = 1000000;
  index->record_boundaries = new vector<record_checkpoint>();
  index->record_chunk_size = RECORD_SPAN;

  size_t record_count = 0;
  if (index->have == 0) {
    fprintf(stderr, "no access points created");
    index->record_boundaries->push_back({0, 0});
    while (iss >> record) {
      auto last_record_start = record.bytes_offset;
      (void)last_record_start;
      ++record_count;
    }
    index->record_boundaries->push_back({record_count, static_cast<uint64_t>(index->length)});
    index->num_record_chunks = 1;
  } else {
    // since `have` >= there must be a first element here
    decltype(index->have) current_access_index = 0;
    point current_access_point = (*index->list)[current_access_index];
    off_t next_decomp_checkpoint = current_access_point.out;
    uint64_t record_start = 0;
    while (iss >> record) {
      record_start = record.bytes_offset;
      if ((record_start >= static_cast<uint64_t>(next_decomp_checkpoint)) and (current_access_index < index->have)) {
        // distance from checkpoint to the record start
        if (index->record_boundaries->size() % 100 == 0) {
          fprintf(stderr, "matched checkpoint %ld with record starting at %ld (record num %ld).\n", next_decomp_checkpoint, record_start, record_count);
        }
        index->record_boundaries->push_back({record_count, record_start});
        current_access_index += 1;
        if (current_access_index < index->have) {
          current_access_point = (*index->list)[current_access_index];
          next_decomp_checkpoint = current_access_point.out;
        }
      }
      ++record_count;
    }
  
    index->record_boundaries->push_back({record_count, static_cast<uint64_t>(index->length)});
    index->num_record_chunks = index->record_boundaries->size();
    index->total_record_count = record_count;
    fprintf(stderr, "Got %ld records boundaries from FASTQ file.\n", record_count);
  }

  // Save index to file
  std::string filename_gzip;
  if (opt_out) {
    filename_gzip = opt_out.value();
  } else {
    filename_gzip = std::string(gzFile1);
    filename_gzip += ".mim";
  }
  fprintf(stderr, "zran: attempting to write index to %s\n", filename_gzip.c_str());
  gzFile idx_gzip = gzopen(filename_gzip.c_str(), "wb");

  // Write the index to the file.
  len = deflate_index_save_gzip(idx_gzip, index);

  if (len != 0) {
    fprintf(stderr, "zran: write error on %s\n", filename_gzip.c_str());
    deflate_index_free(index);
    fclose(in);
    return;
  }
  fprintf(stderr, "zran: wrote index with %d access points to %s\n",
          index->have, filename_gzip.c_str());

  // Clean up and exit
  deflate_index_free(index);
  fclose(in);
  return;
}

bool get_uncompressed_chunk(FILE *fptr, struct deflate_index* index, size_t chunk_idx, std::vector<unsigned char>& buf, uint64_t& expected_rec) {
  if (fptr == nullptr) {
    throw std::runtime_error("zran::get_uncompressed_chunk:: The file pointer is invalid");
  }
  if (chunk_idx >= index->record_boundaries->size()) {
    // user requested an invalid chunk
    return false;
  }

  // otherwise we can get the chunk
  // uncompressed byte offset at the start of the chunk
  off_t chunk_start = (*index->list)[chunk_idx].out;
  (void)chunk_start;
  // uncompressed byte offset at the start of the first read record in this chunk
  uint64_t rec_start = (*index->record_boundaries)[chunk_idx].byte_offset;
  uint64_t rec_count = (*index->record_boundaries)[chunk_idx].first_record_in_chunk;
  // uncompressed byte offset at the start of the first read record in the next chunk
  uint64_t next_rec_start = (*index->record_boundaries)[chunk_idx + 1].byte_offset;
  uint64_t next_rec_count = (*index->record_boundaries)[chunk_idx + 1].first_record_in_chunk;

  expected_rec = next_rec_count - rec_count;
  uint64_t want = next_rec_start - rec_start;
  if (buf.size() < want) {
    buf.resize(want);
  }
  unsigned char* buf_ptr = buf.data();
  // TODO: We know what chunk this is in, so we can avoid the binary search
  // make a variant of this function that takes that hint as well.
  ptrdiff_t got = deflate_index_extract_with_chunk_index(fptr, index, rec_start, static_cast<off_t>(chunk_idx), buf_ptr, want);
  buf.resize(want);

  if (got < 0) {
    fprintf(stderr, "zran: extraction failed: %s error\n",
            got == Z_MEM_ERROR ? "out of memory" : "input corrupted");
    return false;
  } 
  return true; 
}

// TODO: This should be named something else like read_records
std::pair<unsigned char *, int>
read_index(const char *gzFile, struct deflate_index *index, off_t record_idx,
           off_t num_record_chunks, unsigned char *buf = nullptr) {
  FILE *in = fopen(gzFile, "rb");
  if (in == nullptr) {
    throw runtime_error("Could not open the given gzFile for reading");
    return std::make_pair<unsigned char *, int>(nullptr, -1);
  }
  // fprintf(stderr, "Extracting %d record\n", record_idx);

  if (static_cast<uint64_t>(record_idx) >= (index->record_boundaries->size() - num_record_chunks)) {
    num_record_chunks = index->record_boundaries->size() - record_idx - 1;
  }
  /*
  off_t offset = (*index->record_boundaries)[record_idx];
  off_t read_len = (*index->record_boundaries)[record_idx + num_record_chunks] -
                   (*index->record_boundaries)[record_idx];

  */
  off_t offset = 0;
  off_t read_len = 0;
  // TODO: Use shared pointer
  if (buf == nullptr) {
    buf = (unsigned char *)malloc(read_len);
  }
  ptrdiff_t got = deflate_index_extract(in, index, offset, buf, read_len);

  // if (got < 0)
  //     fprintf(stderr, "zran: extraction failed: %s error\n",
  //             got == Z_MEM_ERROR ? "out of memory" : "input corrupted");
  // else {
  //     fwrite(buf, 1, got, stdout);
  // }
  fclose(in);

  return std::make_pair(buf, got);
}

std::pair<unsigned char *, int> read_index(const char *gzFile1,
                                           const char *indexFile,
                                           off_t record_idx,
                                           off_t num_record_chunks) {
  struct deflate_index *index = nullptr;
  int len;

  // fprintf(stderr, "zran: attempting to read index\n");

  FILE *index_file = fopen(indexFile, "rb");
  if (index_file == nullptr) {
    fprintf(stderr, "zran: could not open index for reading\n");
    return std::make_pair<unsigned char *, int>(nullptr, -1);
  }

  gzFile index_file_gzip = gzopen(indexFile, "rb");

  //    len = deflate_index_load(index_file, &index);
  len = deflate_index_load_gzip(index_file_gzip, &index);
  fclose(index_file);

  if (len < 0) {
    switch (len) {
    case Z_MEM_ERROR:
      throw runtime_error("Ran out of memory while building index");
      break;
    case Z_BUF_ERROR:
      throw runtime_error("Building index ended prematurely");
      break;
    case Z_DATA_ERROR:
      throw runtime_error("Saw compressed data error while building index");
      break;
    case Z_ERRNO:
      throw runtime_error("Saw read error while building index");
      break;
    default:
      throw runtime_error("Saw error while building index");
    }
    return std::make_pair<unsigned char *, int>(nullptr, -1);
  }
  fprintf(stderr, "zran: read index with %d access points!\n", len);
  return read_index(gzFile1, index, record_idx, num_record_chunks);
}

#endif //ZRAN_HPP

