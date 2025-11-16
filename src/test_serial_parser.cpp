#include <iostream>
#include <zlib.h>
#include "kseq++/kseq++.hpp"
#include "CLI11.hpp"
#include <chrono>

using namespace klibpp;

struct Bases {
  uint64_t A, C, G, T;
};

int main(int argc, char* argv[]) {
  CLI::App app{"test program for serial parser"};
  argv = app.ensure_utf8(argv);

  std::string fastqFile;
  app.add_option<std::string>("fastq-path", fastqFile, "path to input fastq file.")->required();
  CLI11_PARSE(app, argc, argv);
  auto start = std::chrono::high_resolution_clock::now();

  KSeq seq;
  gzFile fp = gzopen(fastqFile.c_str(), "r");
  auto ks = make_kstream(fp, gzread, mode::in);
  Bases b = {0, 0, 0, 0};
  size_t ctr = 0;
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
