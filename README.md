# `mim` : A small auxiliary index (and parser) to massively speed up parallel parsing of gzipped FASTQ/A files

<img src="https://raw.githubusercontent.com/COMBINE-lab/mim/refs/heads/main/assets/mim.png" width=50% height=50% class="center" alt="Mímir">

Why `mim`? The project's name is a reference to the Norse figure [Mímir](https://en.wikipedia.org/wiki/M%C3%ADmir), who is:

> renowned for his knowledge and wisdom, who is beheaded during the Æsir–Vanir War. Afterward, the god Odin carries around Mímir's head and it recites secret knowledge and counsel to him.

the `mim` index is a small index that gives critical knowledge into the internal structure of a gzipped FASTA/Q file that allows rapid and efficient parallel parsing and decompression.

The purpose of `mim` is so that one can create a `mim` index for gzipped FASTQ files that they anticipate will be reprocessed more than once (e.g. either by themselves or by another party after being deposited in a public database like ENA or SRA).  Having the `mim` index available make subsequent parsing of the data *much* faster, enabling more rapid re-analysis of data (e.g. when new versions of tools or even entirely different analysis algorithms become available).

The `mim` index is purely additive (i.e. creating it does not modify or rewrite any part of the original file), small (typically about 1/1000-th the size of the compressed input file), and takes about as much time to make as simply parsing the input.  This makes it easy to create, store, transfer and share `mim` indexes.


## The Rust implementation

**MSRV**: 1.91. If your Rust version is older than this, please upgrade by running `rustup update`.

### Compiling

The Rust implementation can be found under the `mimrs` directory. It is build using `cargo`, and it is recommended to build it with `target-cpu=native`

```
$ cd mimrs
$ RUSTFLAGS='-C target-cpu=native' cargo b --release
```

This will create the `mim` executable with several sub-commands:

```
Usage: mim <COMMAND>

Commands:
  inspect   look insize an index
  peek      print some reads
  nuc-hist  print some reads
  build     print some reads
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### Building the `mim` index

The `mim` index for a gzipped FASTA/FASTQ file can be built with the `build` sub-command of `mim`

```
Usage: mim build [OPTIONS] --fastq-path <FASTQ_PATH>

Options:
  -f, --fastq-path <FASTQ_PATH>  path to fastx gz input
  -i, --index-path <INDEX_PATH>  optional output path
  -s, --span <SPAN>              span [default: 32000000]
  -m, --metadata <METADATA>      optional metadata
  -h, --help                     Print help
```

For example, to generate an index file using distance between access points of 32 MB

```
./target/release/mim build -f /path/to/compressed-fastq-file 
```

or, if you wanted to embed some useful information in the header

```
./target/release/mim build -f /path/to/compressed-fastq-file --metadata '{ "sample": "that evil fish", "date" : "Nov. 27" }'
```

### Using the `mim` index

To parse a file using the generated index, we provide a sample application:

```
./target/release/mim <fastq_file> <index_file> <nthreads>
```

Right now, this sample application is only a proof of concept.  It simply counts the number of `A`, `C`, `G` and `T` nucleotides in all of the reads in the file.  However, we've build the `mim`-enabled parser to be generic and easy to reuse, so that developers can easily integrate it into their own applications.  Likewise, we are working on build `mim`-enabled parsers in Rust (and Python) that we hope to share here soon!

### Inspecting a constructed `mim` index

You can use the `inspect` command to inspect an existing `mim` index:

```
./target/release/mim inspect <index_file> 
```

## The C++ implementation

### Compiling

The implementation in this repository use the [meson](https://mesonbuild.com/) build system, so you'll need `meson` installed, and [`ninja`](https://ninja-build.org/). Additionally, the current reference implementation is written in `C++`, so you'll need a `C++` compiler (at least capable of `C++17`). The implementation lives under the `cpp` directory, so first, change into that.

```
cd cpp
```

Then you can build the executables


```
# Setup build directory
meson setup builddir

# Or with custom options
meson setup builddir --buildtype=release -Doptimization=3 -Ddebug=true

# Build all targets
meson compile -C builddir

# Or use the shorter ninja command
ninja -C builddir

# Install (installs mimindex and offsets)
meson install -C builddir

# Clean
rm -rf builddir
```

### Building the `mim` index

The `mimindex` executable builds the index. The interface is as below

```
build subcommand
Usage: ./builddir/mimindex build [OPTIONS] fastq-path

Positionals:
  fastq-path TEXT REQUIRED    path to input fastq file.

Options:
  -h,--help                   Print this help message and exit
  --span UINT [32000000]      span of uncompressed input bytes between checkpoints.
  --alt-output TEXT           alternative location to write the mim file (default is input path + ".mim" extension)
  --metadata TEXT Excludes: --metadata-file
                              metadata to embed in the header of the index.
  --metadata-file TEXT:FILE Excludes: --metadata
                              path to JSON file containing metadata to embed in the header of the index.
```

For example, to generate an index file using distance between access points of 64,000,000 bytes

```
./builddir/mimindex build /path/to/compressed-fastq-file 
```

or, if you wanted to embed some useful information in the header

```
./builddir/mimindex build /path/to/compressed-fastq-file --metadata '{ "sample": "that evil fish", "date" : "Nov. 27" }'
```

### Using the `mim` index

To parse a file using the generated index, we provide a sample application:

```
./builddir/test_mim_parser <nthreads> <fastq_file> <index_file> [<fastq_file2>] [<index_file2>]
```

Right now, this sample application is only a proof of concept.  It simply counts the number of `A`, `C`, `G` and `T` nucleotides in all of the reads in the file.  However, we've build the `mim`-enabled parser to be generic and easy to reuse, so that developers can easily integrate it into their own applications.  Likewise, we are working on build `mim`-enabled parsers in Rust (and Python) that we hope to share here soon!

## About kseq++

The parser upon which our `mim`-enabled parser is built it [`kseq++`](https://github.com/cartoonist/kseqpp).

From the `kseq++` website:

> kseq++ is a C++11 re-implementation of [kseq.h](https://github.com/attractivechaos/klib/blob/master/kseq.h). We have
extended its functionality to also compute byte offsets from starting of compressed fastq file, for each record, which
is stored in struct KSeq. 

Additionaly, we have extended its functionality to be able to parse fastq records starting from a specific point in a gzipped file starting at a checkpoint.

#### Note: `mim` started originally as a class project for CMSC701 at the University of Maryland. 

The original approach, which has been altered substantially, was implemented for a final project in the Spring 2025 
edition of CMSC701 at UMD. The original implementation, from which this project eventually evolved, is available 
[here](https://github.com/siddhant-bharti/CMSC701-Project).

