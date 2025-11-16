# `mim` : A small auxiliary index (and parser) to massively speed up parallel parsing of gzipped FASTQ/A files

Why `mim`? The project's name is a reference to the Norse figure [Mímir](https://en.wikipedia.org/wiki/M%C3%ADmir), who is:

> renowned for his knowledge and wisdom, who is beheaded during the Æsir–Vanir War. Afterward, the god Odin carries around Mímir's head and it recites secret knowledge and counsel to him.

the `mim` index is a small index that gives critical knowledge into the internal structure of a gzipped FASTA/Q file that allows rapid and efficient parallel parsing and decompression.

## Building 

```
# Setup build directory
meson setup builddir

# Or with custom options
meson setup builddir -Doptimization=3 -Ddebug=true

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

Generate index file using distance between access points as 32,000,000 bytes 
```
cd $PRJECT_ROOT
./builddir/mimindex build /path/to/compressed-fastq-file 32000000
```

Parsing a file using the generated index
```
cd $PRJECT_ROOT
./builddir/test_mim_parser <nthreads> <fastq_file> <index_file> [<fastq_file2>] [<index_file2>]
```

## Commands to compile various benchmarks

1. FQFeeder
```unix
cd $PRJECT_ROOT
make fqfeeder
./fqfeeder.out /path/to/compressed-fastq-file <num_consumer_threads> <num_parsing_threads>
```

2. ../Scripts/CountBases.cpp
```unix
cd $PROJECT_ROOT
make baseline
./countbases.out /path/to/compressed-fastq-file
```

## Generating test data
```
mkdir test-data
cd test-data

# 1.5 MB compressed
wget https://raw.githubusercontent.com/umd-cmsc701/project_0_test_data/main/test_input/salmonella.fa
gzip salmonella.fa

# 28 MB compressed
wget https://www.be-md.ncbi.nlm.nih.gov/Traces/sra-reads-be/fastq?acc=SRR28596297 -O ecoli.fq.gzip

# 595 MB compressed
wget https://www.be-md.ncbi.nlm.nih.gov/Traces/sra-reads-be/fastq?acc=SRR28592514 -O nematode.fq.gzip
```

## About zran.c

(From https://github.com/madler/zlib/blob/master/examples/)

Illustrate the use of `Z_BLOCK`, `inflatePrime()`, and `inflateSetDictionary()`
for random access of a compressed file. A file containing a raw deflate
stream is provided on the command line. The compressed stream is decoded in
its entirety, and an index built with access points about every `SPAN` bytes
in the uncompressed output. The compressed file is left open, and can then
be read randomly, having to decompress on the average `SPAN`/2 uncompressed
bytes before getting to the desired block of data.

An access point can be created at the start of any deflate block, by saving
the starting file offset and bit of that block, and the 32K bytes of
uncompressed data that precede that block. Also the uncompressed offset of
that block is saved to provide a reference for locating a desired starting
point in the uncompressed stream. `deflate_index_build()` decompresses the
input raw deflate stream a block at a time, and at the end of each block
decides if enough uncompressed data has gone by to justify the creation of a
new access point. If so, that point is saved in a data structure that grows
as needed to accommodate the points.

To use the index, an offset in the uncompressed data is provided, for which
the latest access point at or preceding that offset is located in the index.
The input file is positioned to the specified location in the index, and if
necessary the first few bits of the compressed data is read from the file.
inflate is initialized with those bits and the 32K of uncompressed data, and
decompression then proceeds until the desired offset in the file is reached.
Then decompression continues to read the requested uncompressed data from
the file.

There is some fair bit of overhead to starting inflation for the random
access, mainly copying the 32K byte dictionary. If small pieces of the file
are being accessed, it would make sense to implement a cache to hold some
lookahead to avoid many calls to deflate_index_extract() for small lengths.

Another way to build an index would be to use `inflateCopy()`. That would not
be constrained to have access points at block boundaries, but would require
more memory per access point, and could not be saved to a file due to the
use of pointers in the state. The approach here allows for storage of the
index in a file.

### Structs

- `point` represents an access point
    - Contains preceding 32K of uncompressed data for each access point
- `deflate_index` represents the index that is used to provide fast random access
    - Contains list of all access points

### Original functions

- `deflate_index_build`: builds the index
    - Make one pass through a zlib, gzip, or raw deflate compressed stream and
build an index, with access points about every `span` bytes of uncompressed
output. gzip files with multiple members are fully indexed. `span` should be
chosen to balance the speed of random access against the memory requirements
of the list, which is about 32K bytes per access point. The return value is
the number of access points on success (>= 1), `Z_MEM_ERROR` for out of
memory, `Z_BUF_ERROR` for a premature end of input, `Z_DATA_ERROR` for a format
or verification error in the input file, or `Z_ERRNO` for a file read error.
On success, `*built` points to the resulting index.
- `deflate_index_extract`: uses the index to access `len` amount of data at some random `offset`
    - Use the index to read `len` bytes from `offset` into `buf`. Return the number of
bytes read or a negative error code. If data is requested past the end of
the uncompressed data, then `deflate_index_extract()` will return a value less
than `len`, indicating how much was actually read into `buf`. If given a valid
index, this function should not return an error unless the file was modified
somehow since the index was generated, given that `deflate_index_build()` had
validated all of the input. If nevertheless there is a failure, `Z_BUF_ERROR`
is returned if the compressed data ends prematurely, `Z_DATA_ERROR` if the
deflate compressed data is not valid, `Z_MEM_ERROR` if out of memory,
`Z_STREAM_ERROR` if the index is not valid, or `Z_ERRNO` if there is an error
reading or seeking on the input file.
- `deflate_index_free`: deallocates the index

### Added functions

- `deflate_index_save`: saves index to file
- `deflate_index_load`: loads index from file

## About kseq++

(From https://github.com/cartoonist/kseqpp)

kseq++ is a C++11 re-implementation of [kseq.h](https://github.com/attractivechaos/klib/blob/master/kseq.h). We have
extended its functionality to also compute byte offsets from starting of compressed fastq file, for each record, which
is stored in struct KSeq. Additionaly, we have extended its functionality to be able to parse fastq records from a native
character stream. This allows it to be integrated when we read raw data from a random offset in compressed fastq file. 

#### Note: `mim` started originally as a class project for CMSC701 at the University of Maryland. 

The original approach, which has been altered substantially, was implemented for a final project in the Spring 2025 
edition of CMSC701 at UMD. The original implementation, from which this project eventually evolved, is available 
[here](https://github.com/siddhant-bharti/CMSC701-Project).

