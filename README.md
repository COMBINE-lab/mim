# `mim` : A small auxiliary index (and parser) to massively speed up parallel parsing of gzipped FASTQ/A files

<img src="https://raw.githubusercontent.com/COMBINE-lab/mim/refs/heads/main/assets/mim.png" width=50% height=50%>

Why `mim`? The project's name is a reference to the Norse figure [Mímir](https://en.wikipedia.org/wiki/M%C3%ADmir), who is:

> renowned for his knowledge and wisdom, who is beheaded during the Æsir–Vanir War. Afterward, the god Odin carries around Mímir's head and it recites secret knowledge and counsel to him.

the `mim` index is a small index that gives critical knowledge into the internal structure of a gzipped FASTA/Q file that allows rapid and efficient parallel parsing and decompression.

## Building 

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

## Running this project

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
./builddir/mimindex build /path/to/compressed-fastq-file --span 32000000
```

To parse a file using the generated index:

```
./builddir/test_mim_parser <nthreads> <fastq_file> <index_file> [<fastq_file2>] [<index_file2>]
```

## About kseq++

(From https://github.com/cartoonist/kseqpp)

kseq++ is a C++11 re-implementation of [kseq.h](https://github.com/attractivechaos/klib/blob/master/kseq.h). We have
extended its functionality to also compute byte offsets from starting of compressed fastq file, for each record, which
is stored in struct KSeq. Additionaly, we have extended its functionality to be able to parse fastq records starting from 
a specific point in a gzipped file starting at a checkpoint.

#### Note: `mim` started originally as a class project for CMSC701 at the University of Maryland. 

The original approach, which has been altered substantially, was implemented for a final project in the Spring 2025 
edition of CMSC701 at UMD. The original implementation, from which this project eventually evolved, is available 
[here](https://github.com/siddhant-bharti/CMSC701-Project).

