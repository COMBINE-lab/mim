#include "parser.hpp"
#include "indicators.hpp"
#include "CLI11.hpp"
#include <iostream>
#include <thread>
#include <vector>
#include <chrono>
using namespace std;

struct Bases {
  uint64_t A, C, G, T;
};

Bases do_count_paired_end(std::string& fastqFile, std::string& indexFile, std::string& fastqFile2, std::string& indexFile2, size_t nt, size_t& ctr_out) {
  ParrFQParser<ReadPairChunk> parser;
  parser.init_pair(fastqFile, indexFile, fastqFile2, indexFile2, nt);

  cout << "Starting parsing" << endl;
  parser.start();

  cout << "Parsers Started" << endl;

  using namespace indicators;

  // Hide cursor
  show_console_cursor(false);

  indicators::ProgressBar bar{
    option::BarWidth{50},
    option::Start{" ["},
    option::Fill{"█"},
    option::Lead{"█"},
    option::Remainder{"-"},
    option::End{"]"},
    option::MaxProgress{parser.get_num_reads() / 1000},
    option::ForegroundColor{Color::yellow},
    option::ShowElapsedTime{true},
    option::ShowRemainingTime{true},
    option::FontStyles{std::vector<FontStyle>{FontStyle::bold}}
  };

  std::cerr << "NUMBER OF READS " << parser.get_num_reads() << "\n";

  std::vector<std::thread> readers;
  std::vector<Bases> counters(nt, {0, 0, 0, 0});
  std::atomic<size_t> ctr{0};
  for (size_t i = 0; i < nt; ++i) {
    readers.emplace_back([&, i]() {
      auto rg = parser.get_read_chunk();
      if (!rg) {
        return 1;
      }
      ReadPair seq;
      uint64_t cur_rec{0};
      while (*rg >> seq) { 
        if (cur_rec > 0 && cur_rec % 1000 == 0) { bar.tick(); }
        //std::cerr << "rec : " << j << " / " << expected_rec << "\n";
        ++cur_rec;
        for (size_t j = 0; j < seq.first.seq.length(); ++j) {
          char c = seq.first.seq[j];
          switch (c) {
            case 'A':
              counters[i].A++;
              break;
            case 'C':
              counters[i].C++;
              break;
            case 'G':
              counters[i].G++;
              break;
            case 'T':
              counters[i].T++;
              break;
            default:
              break;
          }
        }
        for (size_t j = 0; j < seq.second.seq.length(); ++j) {
          char c = seq.second.seq[j];
          switch (c) {
            case 'A':
              counters[i].A++;
              break;
            case 'C':
              counters[i].C++;
              break;
            case 'G':
              counters[i].G++;
              break;
            case 'T':
              counters[i].T++;
              break;
            default:
              break;
          }
        }
      }

      ctr += cur_rec; 
      cur_rec = 0;
      return 0;
    });
  }

  for (auto& t : readers) {
    t.join();
  }
  bar.mark_as_completed();
  parser.stop();
  // Show cursor
  indicators::show_console_cursor(true);
  ctr_out = ctr;
  Bases b = {0, 0, 0, 0};
  for (size_t i = 0; i < nt; ++i) {
    b.A += counters[i].A;
    b.C += counters[i].C;
    b.G += counters[i].G;
    b.T += counters[i].T;
  }
  return b;
}

Bases do_count_single_end(std::string& fastqFile, std::string& indexFile, size_t nt, size_t& ctr_out) {
  ParrFQParser<ReadChunk> parser;
  parser.init(fastqFile, indexFile, nt);

  cout << "Starting parsing" << endl;
  parser.start();

  cout << "Parsers Started" << endl;

  using namespace indicators;

  // Hide cursor
  show_console_cursor(false);

  indicators::ProgressBar bar{
    option::BarWidth{50},
    option::Start{" ["},
    option::Fill{"█"},
    option::Lead{"█"},
    option::Remainder{"-"},
    option::End{"]"},
    option::MaxProgress{parser.get_num_reads() / 1000},
    option::ForegroundColor{Color::yellow},
    option::ShowElapsedTime{true},
    option::ShowRemainingTime{true},
    option::FontStyles{std::vector<FontStyle>{FontStyle::bold}}
  };

  std::cerr << "NUMBER OF READS " << parser.get_num_reads() << "\n";

  std::vector<std::thread> readers;
  std::vector<Bases> counters(nt, {0, 0, 0, 0});
  std::atomic<size_t> ctr{0};
  for (size_t i = 0; i < nt; ++i) {
    readers.emplace_back([&, i]() {
      auto rg = parser.get_read_chunk();
      if (!rg) {
        return 1;
      }
      klibpp::KSeq seq;
      uint64_t cur_rec{0};
      while (*rg >> seq) { 
        if (cur_rec > 0 && cur_rec % 1000 == 0) { bar.tick(); }
        //std::cerr << "rec : " << j << " / " << expected_rec << "\n";
        ++cur_rec;
        for (size_t j = 0; j < seq.seq.length(); ++j) {
          char c = seq.seq[j];
          switch (c) {
            case 'A':
              counters[i].A++;
              break;
            case 'C':
              counters[i].C++;
              break;
            case 'G':
              counters[i].G++;
              break;
            case 'T':
              counters[i].T++;
              break;
            default:
              break;
          }
        }
      }

      ctr += cur_rec; 
      cur_rec = 0;
      return 0;
    });
  }

  for (auto& t : readers) {
    t.join();
  }
  bar.mark_as_completed();
  parser.stop();
  // Show cursor
  indicators::show_console_cursor(true);
  ctr_out = ctr;
  Bases b = {0, 0, 0, 0};
  for (size_t i = 0; i < nt; ++i) {
    b.A += counters[i].A;
    b.C += counters[i].C;
    b.G += counters[i].G;
    b.T += counters[i].T;
  }
  return b;
}

int main(int argc, char* argv[]) {
  CLI::App app{"test program for ffparser"};
  argv = app.ensure_utf8(argv);
 
  std::string fastqFile;
  std::string indexFile;
  std::string fastqFile2;
  std::string indexFile2;
  size_t nt{4};
  app.add_option<size_t>("num-threads", nt, "number of parsing threads to use.")->required();
  app.add_option<std::string>("fastq-path", fastqFile, "path to input fastq file.")->required();
  app.add_option<std::string>("fastq-index-path", indexFile, "path to input fastq file index.")->required();

  app.add_option<std::string>("fastq-path2", fastqFile2, "path to input fastq file 2.");
  app.add_option<std::string>("fastq-index-path2", indexFile2, "path to input fastq file index 2.");
  CLI11_PARSE(app, argc, argv);

  auto start = std::chrono::high_resolution_clock::now();
  size_t ctr = 0;
  Bases b;
  std::string read_desc;
  if (fastqFile2.size() == 0) {
    b = do_count_single_end(fastqFile, indexFile, nt, ctr);
    read_desc = "reads";
  } else {
    b = do_count_paired_end(fastqFile, indexFile, fastqFile2, indexFile2, nt, ctr);
    read_desc = "read pairs";
  }

  std::cerr << "\n";
  std::cerr << "Parsed " << ctr << " total " << read_desc << ".\n";
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
