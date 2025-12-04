#pragma once
#include <zlib.h>
#include <sstream>

#include "kseq++/kseq++.hpp"
#include "zran_index_helpers.hpp"

// Custom stream reader for reading from a character buffer instead of a file
class KseqIndexedGzipStreamIn : public klibpp::KStreamIn<GzipStreamReader*, ptrdiff_t(*)(GzipStreamReader*, char*, size_t)> {
 public:
  using Base = klibpp::KStreamIn<GzipStreamReader*, ptrdiff_t(*)(GzipStreamReader*, char*, size_t)>;

  KseqIndexedGzipStreamIn(GzipStreamReader* reader) : Base(reader, do_gzip_read, close) {}

  static ptrdiff_t do_gzip_read(GzipStreamReader* reader, char* data, size_t size) {
    return gzipstream_read(reader, data, size);
  }

  static int close(GzipStreamReader* reader) {
    return 0;
  }
};

