#include "parser.hpp"
#include "indicators.hpp"
#include "CLI11.hpp"
#include <iostream>
#include <thread>
#include <vector>
#include <chrono>
using namespace std;

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

Bases do_count_paired_end(std::string& fastqFile, std::string& indexFile, std::string& fastqFile2, std::string& indexFile2, size_t nt, size_t& ctr_out) {
  ParrFQParser<ReadPairChunk> parser;
  parser.init_pair(fastqFile, indexFile, fastqFile2, indexFile2, nt);

  cout << "Starting parsing" << endl;
  parser.start();

  cout << "Parsers Started" << endl;

  using namespace indicators;

  std::atomic<size_t> maxticks = parser.get_num_reads() / 100000;
  std::atomic<size_t> nticks{0};
  // Hide cursor
  show_console_cursor(false);

  indicators::ProgressBar bar{
    option::BarWidth{50},
    option::Start{" ["},
    option::Fill{"█"},
    option::Lead{"█"},
    option::Remainder{"-"},
    option::End{"]"},
    option::MaxProgress{maxticks},
    option::ForegroundColor{Color::yellow},
    option::ShowElapsedTime{true},
    option::ShowRemainingTime{true},
    option::FontStyles{std::vector<FontStyle>{FontStyle::bold}}
  };

  std::cerr << "NUMBER OF READS " << parser.get_num_reads() << "\n";
  
  std::vector<std::thread> readers;
  std::vector<Counters> counters(nt, {0, 0, 0, 0});
  std::atomic<size_t> ctr{0};

  for (size_t i = 0; i < nt; ++i) {
    readers.emplace_back([&, i]() {
      auto rgo = parser.get_read_chunk();
      if (!rgo) {
        return 1;
      }
      auto rg = std::move(rgo.value());
      ReadPair seq;
      uint64_t cur_rec{0};
      while (rg >> seq) { 
        if (cur_rec % 100000 == 0) { 
          ++nticks; 
          if (nticks < maxticks) { bar.tick(); } 
        } 
        ++cur_rec;
        for (unsigned char c : seq.first.seq) {
			counters[i].counts[(c >> 1) & 3] += 1;
        }
        for (unsigned char c : seq.second.seq) {
			counters[i].counts[(c >> 1) & 3] += 1;
        }
      }

      ctr += cur_rec; 
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
    b.A += counters[i].counts[0];//.A;
    b.C += counters[i].counts[1];//.C;
    b.G += counters[i].counts[2];//.G;
    b.T += counters[i].counts[3];//.T;
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

  std::cerr << "NUMBER OF READS " << parser.get_num_reads() << "\n";
  // Hide cursor
  show_console_cursor(false);

  

  std::vector<Counters> counters(nt, {0, 0, 0, 0});
  std::atomic<size_t> ctr{0};
  std::atomic<size_t> nticks{0};
  std::atomic<size_t> maxticks = parser.get_num_reads() / 100000;
  {
    /*
    indicators::ProgressBar bar{
      option::BarWidth{50},
      option::Start{" ["},
      option::Fill{"█"},
      option::Lead{"█"},
      option::Remainder{"-"},
      option::End{"]"},
      option::MaxProgress{maxticks.load()},
      option::ForegroundColor{Color::yellow},
      option::ShowElapsedTime{true},
      option::ShowRemainingTime{true},
      option::FontStyles{std::vector<FontStyle>{FontStyle::bold}}
    };
    */

    std::vector<std::thread> readers;
    readers.reserve(nt);
    for (size_t i = 0; i < nt; ++i) {
      auto& counter = counters[i];
      readers.emplace_back([&, i]() {
        auto rgo = parser.get_read_chunk();
        if (!rgo) {
          return 1;
        }
        auto rg = std::move(rgo.value());
        klibpp::KSeq seq;
        uint64_t cur_rec{0};
        while (rg >> seq) { 
          /*
          if (cur_rec % 100000 == 0) { 
            ++nticks; 
            if (nticks < maxticks) { bar.tick(); } 
          };
          */
          ++cur_rec;
          for (unsigned char c : seq.seq) {
			counter.counts[(c >> 1) & 3] += 1;
          }
        }
        ctr += cur_rec; 
        return 0;
      });
    }

    for (auto& t : readers) {
      t.join();
    }
    //bar.mark_as_completed();
  }
  parser.stop();
  // Show cursor
  //indicators::show_console_cursor(true);
  ctr_out = ctr;
  Bases b = {0, 0, 0, 0};
  for (size_t i = 0; i < nt; ++i) {
    b.A += counters[i].counts[0];
    b.C += counters[i].counts[1];
    b.G += counters[i].counts[2];
    b.T += counters[i].counts[3];
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
  std::cerr << "#T (or N) = " << b.T << '\n';
  auto end = std::chrono::high_resolution_clock::now();

  // Calculate the duration in milliseconds
  auto duration2 = std::chrono::duration_cast<std::chrono::milliseconds>(end - start);

  // Output the duration
  std::cout << "Time taken (total): " << duration2.count() << " milliseconds" << std::endl;
  return 0;
}
