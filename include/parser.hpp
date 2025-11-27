#pragma once
#include "concurrentqueue/concurrentqueue.h"
#include "kseq++/seqio.hpp"
#include "zran.hpp"
#include "json.hpp"
#include "zran_index_helpers.hpp"
#include "kseqcharstream.hpp"
#include <atomic>
#include <functional>
#include <memory>
#include <stdio.h>
#include <thread>
#include <vector>
#include <optional>


// Represents a contiguous chunk [start, end)
struct Chunk {
    size_t start;
    size_t end;
    
    size_t size() const { return end - start; }
    bool empty() const { return size() == 0; }
};


class ReadChunk {
public:
  ReadChunk(std::string fastq_file_name, struct deflate_index* idx, uint64_t token, Chunk chunk) : token_(token), chunk_range_(chunk) {
    idx_ptr_ = new struct deflate_index(*idx);
    size_t start_chunk = chunk_range_.start;
    size_t end_chunk = chunk_range_.end;
    reader_ = open_gzip_stream_at_checkpoint(fastq_file_name.c_str(), idx_ptr_, start_chunk);

    // get the byte offset of the first read in this chunk
    auto& rec_boundaries = *idx_ptr_->record_boundaries;
    uint64_t first_read_byte = rec_boundaries.at(start_chunk).byte_offset;
    uint64_t first_record_id = rec_boundaries.at(start_chunk).first_record_in_chunk;

    // we want to discard bytes from the current uncompressed_offset until the first_read_byte
    off_t amount_to_discard = first_read_byte - reader_->uncompressed_offset;
    // if we don't have to discard anything, avoid all of this work
    if (amount_to_discard != 0) { 
      std::vector<char> discard_buffer(amount_to_discard); 
      ptrdiff_t bytes_read = gzipstream_read(reader_.get(), const_cast<char*>(discard_buffer.data()), amount_to_discard);
      if (bytes_read < 0) {
        std::cerr << "WORKER " << token_ << ", ATTEMPTING TO DISCARD INITIAL " << amount_to_discard 
                  << " BYTES; ERROR OPENING AND READING FROM CHECKPOINT " << start_chunk << "\n";
        return;
      }
    } 

    current_rec_ = first_record_id;
    uint64_t last_record_id = (end_chunk+1 < rec_boundaries.size()) ? rec_boundaries.at(end_chunk).first_record_in_chunk : idx_ptr_->total_record_count;
    last_rec_ = last_record_id;
    in_stream_.reset(new KseqIndexedGzipStreamIn(reader_.get()));
  }
 
  ReadChunk(ReadChunk&& other) = default;
  ReadChunk(const ReadChunk& other) = delete;
  ReadChunk& operator=(const ReadChunk& other) = delete;
  ReadChunk& operator=(ReadChunk&& other) = delete;
  
  inline KseqIndexedGzipStreamIn& operator>>( klibpp::KSeq& rec )  {
    return (current_rec_ < last_rec_) ? ((*in_stream_) >> rec, current_rec_++, *in_stream_) : (in_stream_->set_eof(), *in_stream_);
  }
  struct deflate_index* idx_ptr_{nullptr}; // this reader's local file ptr
  // the gzFile we're currently holding
  std::unique_ptr<GzipStreamReader> reader_;
  std::unique_ptr<KseqIndexedGzipStreamIn> in_stream_{nullptr};
  // the number of records we expect to parse from
  // this chunk.
  uint64_t current_rec_;
  uint64_t last_rec_;
  // the unique token identifying the worker
  // to which this ReadChunk belongs
  uint64_t token_;
  Chunk chunk_range_;
};

struct ReadPair {
  klibpp::KSeq first;
  klibpp::KSeq second;
};

class ReadPairChunk {
public:
  ReadPairChunk(std::string fastq_file_name, struct deflate_index* idx, 
                std::string fastq_file_name2, struct deflate_index* idx2,
                uint64_t token, Chunk chunk) : token_(token), chunk_range_(chunk) {
    idx_ptr_ = new struct deflate_index(*idx);
    size_t start_chunk = chunk_range_.start;
    size_t end_chunk = chunk_range_.end;
    reader_ = open_gzip_stream_at_checkpoint(fastq_file_name.c_str(), idx_ptr_, start_chunk);

    // get the byte offset of the first read in this chunk
    auto& rec_boundaries = *idx_ptr_->record_boundaries;
    uint64_t first_read_byte = rec_boundaries.at(start_chunk).byte_offset;
    uint64_t first_record_id = rec_boundaries.at(start_chunk).first_record_in_chunk;

    // we want to discard bytes from the current uncompressed_offset until the first_read_byte
    off_t amount_to_discard = first_read_byte - reader_->uncompressed_offset;
    // if we don't have to discard anything, avoid all of this work
    if (amount_to_discard != 0) { 
      std::vector<char> discard_buffer(amount_to_discard); 
      ptrdiff_t bytes_read = gzipstream_read(reader_.get(), const_cast<char*>(discard_buffer.data()), amount_to_discard);
      if (bytes_read < 0) {
        std::cerr << "WORKER " << token_ << ", ATTEMPTING TO DISCARD INITIAL " << amount_to_discard 
                  << " BYTES; ERROR OPENING AND READING FROM CHECKPOINT " << start_chunk << "\n";
        return;
      }
    } 
    current_rec_ = first_record_id;
    uint64_t last_record_id = (end_chunk+1 < rec_boundaries.size()) ? rec_boundaries.at(end_chunk).first_record_in_chunk : idx_ptr_->total_record_count;
    last_rec_ = last_record_id;

    in_stream_.reset(new KseqIndexedGzipStreamIn(reader2_.get()));

    // now, synchronize the second file with the first, that is, first, find the chunk in the second file 
    // containing the closest read ID less than or equal to first_record_id
    idx_ptr2_ = new struct deflate_index(*idx2);

    size_t boundary_index = 0;
    auto& rec_boundaries2 = *idx_ptr2_->record_boundaries;
    if (first_record_id > 0 && rec_boundaries2.size() > 1) {
      // linear search for now
      for (auto rbi = rec_boundaries2.begin() + 1; rbi != rec_boundaries2.end(); ++rbi) {
        if (rbi->first_record_in_chunk >= first_record_id) {
          break;
        } else {
          boundary_index++;
        }
      }
    }

    uint64_t pair_starting_byte = rec_boundaries2[boundary_index].byte_offset;
    uint64_t pair_starting_id = rec_boundaries2[boundary_index].first_record_in_chunk;

    reader2_ = open_gzip_stream_at_checkpoint(fastq_file_name2.c_str(), idx_ptr2_, boundary_index);
    // we want to discard bytes from the current uncompressed_offset until the first_read_byte
    off_t amount_to_discard2 = pair_starting_byte - reader2_->uncompressed_offset;
    // if we don't have to discard anything, avoid all of this work
    if (amount_to_discard2 != 0) { 
      std::vector<char> discard_buffer(amount_to_discard2); 
      ptrdiff_t bytes_read = gzipstream_read(reader2_.get(), const_cast<char*>(discard_buffer.data()), amount_to_discard2);
      if (bytes_read < 0) {
        std::cerr << "WORKER " << token_ << ", ATTEMPTING TO DISCARD INITIAL " << amount_to_discard2 
                  << " BYTES; ERROR OPENING AND READING FROM CHECKPOINT " << boundary_index << "\n";
        return;
      }
    } 

    uint64_t num_reads_to_skip = first_record_id - pair_starting_id;
    klibpp::KSeq discard;
    in_stream2_.reset(new KseqIndexedGzipStreamIn(reader2_.get()));
    for (size_t i = 0; i < num_reads_to_skip; ++i) {
      (*in_stream2_) >> discard;
    }
    // now the streams should be synced
  }
 
  ReadPairChunk(ReadPairChunk&& other) = default;
  ReadPairChunk(const ReadPairChunk& other) = delete;
  ReadPairChunk& operator=(const ReadPairChunk& other) = delete;
  ReadPairChunk& operator=(ReadPairChunk&& other) = delete;
  
  inline KseqIndexedGzipStreamIn& operator>>(ReadPair& rec)  {
    return (current_rec_ < last_rec_) ? (
      (*in_stream_) >> rec.first,
      (*in_stream2_) >> rec.second,
      current_rec_++,
      *in_stream_) : (
      in_stream_->set_eof(),
      in_stream2_->set_eof(),
      *in_stream_
      );
  }
 
  struct deflate_index* idx_ptr_{nullptr}; // this reader's local file ptr
  struct deflate_index* idx_ptr2_{nullptr}; // this reader's local file ptr

  // this thread's local reader
  std::unique_ptr<GzipStreamReader> reader_{nullptr};
  std::unique_ptr<GzipStreamReader> reader2_{nullptr};

  std::unique_ptr<KseqIndexedGzipStreamIn> in_stream_{nullptr};
  std::unique_ptr<KseqIndexedGzipStreamIn> in_stream2_{nullptr};

  // the number of records we expect to parse from
  // this chunk.
  uint64_t current_rec_;
  uint64_t last_rec_;
  // the unique token identifying the worker
  // to which this ReadChunk belongs
  uint64_t token_;
  // 
  Chunk chunk_range_;
};

template <typename ReadChunkT>
class ParrFQParser {
public:
  ParrFQParser()
      : m_index(nullptr,
                [](struct deflate_index *p) { if (p!= nullptr) { deflate_index_free(p); } }),
       m_index2(nullptr,
                [](struct deflate_index *p) { if (p!= nullptr) { deflate_index_free(p); } }) {}

  ~ParrFQParser();

  int init(const std::string &fastqFilename, const std::string &indexFileName, uint64_t num_consumers);

  int init_pair(const std::string &fastqFilename, const std::string &indexFileName,
                const std::string &fastqFilename2, const std::string &indexFileName2, uint64_t num_consumers);



  // Start and stop the parser
  int start();
  int stop();
  uint64_t get_num_chunks();
  uint64_t get_num_reads();

  // Consumer functions
  std::optional<ReadChunkT> get_read_chunk();
private:
  std::unique_ptr<struct deflate_index,
                  std::function<void(struct deflate_index *)>>
      m_index;
  std::unique_ptr<struct deflate_index,
                  std::function<void(struct deflate_index *)>>
      m_index2;

  uint64_t num_consumers_; // number of threads that will consume reads
  std::string m_fastqFilename;
  std::string m_indexFileName;
  std::string m_fastqFilename2;
  std::string m_indexFileName2;
  bool m_isRunning = false;
  std::atomic_uint64_t chunk_counter_{0};
  std::atomic_uint64_t token_counter_{0};
  std::vector<Chunk> chunk_ranges_;
  // Helper functions
  int loadIndex(const std::string &indexFileName, std::unique_ptr<struct deflate_index, std::function<void(struct deflate_index *)>>& ptr);
  uint64_t getMaxBufLen();
};

#include "parser.inl"
