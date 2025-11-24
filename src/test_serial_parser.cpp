#include <iostream>
#include <zlib.h>
#include "kseq++/kseq++.hpp"
#include "CLI11.hpp"
#include <chrono>

using namespace klibpp;

struct Bases {
  uint64_t A, C, G, T;
};

Bases parse_file_pair(const std::string& fname, const std::string& fname2, size_t& ctr) {
  KSeq seq;
  KSeq seq2;
  gzFile fp = gzopen(fname.c_str(), "r");
  gzFile fp2 = gzopen(fname2.c_str(), "r");

  auto ks = make_kstream(fp, gzread, mode::in);
  auto ks2 = make_kstream(fp2, gzread, mode::in);

  Bases b = {0, 0, 0, 0};
  while ((ks >> seq) && (ks2 >> seq2)) { 
    for (size_t j = 0; j < seq.seq.length(); ++j) {
      char c = seq.seq[j];
      switch (c) {
        case 'A':
          b.A++;
          break;
        case 'C':
          b.C++;
          break;
        case 'G':
          b.G++;
          break;
        case 'T':
          b.T++;
          break;
        default:
          break;
      }
    }
    for (size_t j = 0; j < seq2.seq.length(); ++j) {
      char c = seq2.seq[j];
      switch (c) {
        case 'A':
          b.A++;
          break;
        case 'C':
          b.C++;
          break;
        case 'G':
          b.G++;
          break;
        case 'T':
          b.T++;
          break;
        default:
          break;
      }
    }
    ++ctr;
  }
  return b;
}

Bases parse_single_file(const std::string& fname, size_t& ctr) {
  KSeq seq;
  gzFile fp = gzopen(fname.c_str(), "r");
  auto ks = make_kstream(fp, gzread, mode::in);
  Bases b = {0, 0, 0, 0};
  while (ks >> seq) { 
    for (size_t j = 0; j < seq.seq.length(); ++j) {
      char c = seq.seq[j];
      switch (c) {
        case 'A':
          b.A++;
          break;
        case 'C':
          b.C++;
          break;
        case 'G':
          b.G++;
          break;
        case 'T':
          b.T++;
          break;
        default:
          break;
      }
    }
    ++ctr;
  }
  return b;
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
