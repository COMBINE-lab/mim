#include "CLI11.hpp"
#include "kseq++/seqio.hpp"
#include <chrono>
#include <iostream>
#include <optional>
#include "zran.hpp"
using namespace std;
using namespace klibpp;

int main(int argc, char **argv) {
  CLI::App app{"Build a mim index"};
  argv = app.ensure_utf8(argv);

  std::string fastqFile;
  std::string metadata_string;
  std::string metadata_file;
  std::string alt_out;
  size_t span = 32'000'000;
  CLI::App* build = app.add_subcommand("build", "build subcommand");
  build->add_option<std::string>("fastq-path", fastqFile, "path to input fastq file.")->required();
  build->add_option<size_t>("--span", span, "span of uncompressed input bytes between checkpoints.")->capture_default_str();
  
  auto output_opt = build->add_option<std::string>("--alt-output", alt_out, "alternative location to write the mim file (default is input path + \".mim\" extension)");
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

    std::optional<std::string> alt_out_path = (*output_opt) ? std::optional<std::string>(alt_out_path) : std::nullopt;

    // std::cout << "metadata = " << j.dump(4) << "\n";
    // build mode
    auto start = std::chrono::high_resolution_clock::now();
    build_index(fastqFile.c_str(), span, std::move(j), alt_out_path);
    auto end = std::chrono::high_resolution_clock::now();

    // Calculate the duration in milliseconds
    auto duration2 =
        std::chrono::duration_cast<std::chrono::milliseconds>(end - start);

    // Output the duration
    std::cout << "Time taken to build index (total): " << duration2.count()
              << " milliseconds" << std::endl;

  } 
  return 0;
}
