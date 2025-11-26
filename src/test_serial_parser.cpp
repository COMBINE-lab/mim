#include <iostream>
#include <zlib.h>
#include "kseq++/kseq++.hpp"
#include "CLI11.hpp"
#include <chrono>

using namespace klibpp;

struct Bases {
  alignas(64) uint64_t A;
  uint64_t C;
  uint64_t G;
  uint64_t T;
};

struct Counters {
  alignas(64) std::array<uint64_t, 4> counts; 
};

// Lookup table: maps ASCII char to index (0=A, 1=C, 2=G, 3=T, -1=other)
static constexpr int8_t lookup[256] = {
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1, 0,-1, 1,-1,-1,-1, 2,-1,-1,-1,-1,-1,-1,-1,-1, // @ABCDEFGHIJKLMNO
  -1,-1,-1,-1, 3,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1, // PQRSTUVWXYZ
  -1, 0,-1, 1,-1,-1,-1, 2,-1,-1,-1,-1,-1,-1,-1,-1, // `abcdefghijklmno
  -1,-1,-1,-1, 3,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1, // pqrstuvwxyz
  // Rest are -1
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
  -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1
};


Bases parse_file_pair(const std::string& fname, const std::string& fname2, size_t& ctr) {
  KSeq seq;
  KSeq seq2;
  gzFile fp = gzopen(fname.c_str(), "r");
  gzFile fp2 = gzopen(fname2.c_str(), "r");

  auto ks = make_kstream(fp, gzread, mode::in);
  auto ks2 = make_kstream(fp2, gzread, mode::in);

  Counters counter = {0, 0, 0, 0};
  while ((ks >> seq) && (ks2 >> seq2)) { 
    for (unsigned char c : seq.seq) {
      int idx = lookup[c];
      if (idx >= 0) { counter.counts[idx]++; }
    }
    for (unsigned char c : seq2.seq) {
      int idx = lookup[c];
      if (idx >= 0) { counter.counts[idx]++; }
    }
    ++ctr;
  }
  return Bases{counter.counts[0], counter.counts[1], counter.counts[2], counter.counts[3]};
}

Bases parse_single_file(const std::string& fname, size_t& ctr) {
  KSeq seq;
  gzFile fp = gzopen(fname.c_str(), "r");
  auto ks = make_kstream(fp, gzread, mode::in);
  Counters counter = {0, 0, 0, 0};
  while (ks >> seq) { 
    for (unsigned char c : seq.seq) {
      int idx = lookup[c];
      if (idx >= 0) { counter.counts[idx]++; }
    }
    ++ctr;
  }
  return Bases{counter.counts[0], counter.counts[1], counter.counts[2], counter.counts[3]};
}

int main(int argc, char* argv[]) {
  CLI::App app{"test program for serial parser"};
  argv = app.ensure_utf8(argv);

  std::string fastqFile;
  std::string fastqFile2;
  app.add_option<std::string>("fastq-path", fastqFile, "path to input fastq file.")->required();
  app.add_option<std::string>("fastq-path2", fastqFile2, "path to input fastq file 2.");
  CLI11_PARSE(app, argc, argv);
  auto start = std::chrono::high_resolution_clock::now();

  Bases b;
  size_t ctr = 0;
  if (fastqFile2.empty()) {
    b = parse_single_file(fastqFile, ctr);
  } else {
    b = parse_file_pair(fastqFile, fastqFile2, ctr);
  }
  std::cerr << "\n";
  std::cerr << "Parsed " << ctr << " total read pairs.\n";
  std::cerr << "\n#A = " << b.A << '\n';
  std::cerr << "#C = " << b.C << '\n';
  std::cerr << "#G = " << b.G << '\n';
  std::cerr << "#T = " << b.T << '\n';
  auto end = std::chrono::high_resolution_clock::now();

  // Calculate the duration in milliseconds
  auto duration2 = std::chrono::duration_cast<std::chrono::milliseconds>(end - start);

  // Output the duration
  std::cout << "Time taken (total): " << duration2.count() << " milliseconds" << std::endl;
  return 0;
}
