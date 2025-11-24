#include "CLI11.hpp"
#include "kseq++/seqio.hpp"
#include "kseqcharstream.hpp"
#include <chrono>
#include <iostream>
#include "zran.hpp"
using namespace std;
using namespace klibpp;

int main(int argc, char **argv) {
  CLI::App app{"Build and use ffindices"};
  argv = app.ensure_utf8(argv);

  std::string fastqFile;
  std::string metadata_string;
  std::string metadata_file;
  size_t span = 25'000'000;
  CLI::App* build = app.add_subcommand("build", "build subcommand");
  build->add_option<std::string>("fastq-path", fastqFile, "path to input fastq file.")->required();
  build->add_option<size_t>("span", span, "span of uncompressed input bytes between checkpoints.")->required();
  
  auto metadata_opt = build->add_option<std::string>("--metadata", metadata_string, "metadata to embed in the header of the index.");
  auto metadata_file_opt = build->add_option<std::string>("--metadata-file", 
                                                          metadata_file, 
                                                          "path to JSON file containing metadata to embed in the header of the index.")->check(CLI::ExistingFile);
  metadata_opt->excludes(metadata_file_opt);

  CLI11_PARSE(app, argc, argv);

  {
    using json = nlohmann::json;
    json j;

    if (*metadata_file_opt) {
      std::ifstream f(metadata_file);
      j = json::parse(f);
    } else if (*metadata_opt) {
      j = json::parse(metadata_string);
    }
    // std::cout << "metadata = " << j.dump(4) << "\n";
    // build mode
    auto start = std::chrono::high_resolution_clock::now();
    build_index(fastqFile.c_str(), span, std::move(j));
    auto end = std::chrono::high_resolution_clock::now();

    // Calculate the duration in milliseconds
    auto duration2 =
        std::chrono::duration_cast<std::chrono::milliseconds>(end - start);

    // Output the duration
    std::cout << "Time taken to build index (total): " << duration2.count()
              << " milliseconds" << std::endl;

  } 
  /*{
    // use mode
    off_t record_idx = -1;
    off_t num_records = -1;
    if (argc > 2) {
      char *end;
      record_idx = strtoll(argv[4], &end, 0);
      if (*end) {
        fprintf(stderr, "zran: invalid record_idx\n");
        return 1;
      }

      char *end1;
      num_records = strtoll(argv[5], &end1, 0);
      if (*end1) {
        fprintf(stderr, "zran: invalid num_records\n");
        return 1;
      }
    }
    unsigned char *buf;
    int got;
    std::tie(buf, got) = read_index(argv[2], argv[3], record_idx, num_records);
    if (got < 0)
      fprintf(stderr, "zran: extraction failed: %s error\n",
              got == Z_MEM_ERROR ? "out of memory" : "input corrupted");
    else {
      fwrite(buf, 1, got, stdout);
    }
    KseqCharStreamIn in(reinterpret_cast<const char *>(buf), got);

    //        klibpp::KSeq rec;
    //        while (in >> rec) {
    //          cout << rec.seq << endl;
    //        }
  }
  */
  return 0;
}
