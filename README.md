# `mim` : A small auxiliary index (and parser) to massively speed up parallel parsing of gzipped FASTQ/A files

Tl;dr: Mim is a small 0.1% overhead index alongside `.fastq.gz` files that allows for
multithreaded decompression, speeding up analysis pipelines.

``` sh
# Build and install mim
cargo install mim-index
# Build an index file `records.fastq.gz.mim`
# Takes about as long as decompression on a single thread.
mim build records.fastq.gz
# Decompress the file into 8 `.fastq.{0..8}` files in parallel.
mim unzip records.fastq.gz --parts 8
# Decompress into 8 named pipes instead, that can be ingested once.
mim unzip records.fastq.gz --parts 8 --pipe
```

*Disclaimer: while the `.mim` index format is stable for now, the CLI needs
further polishing. Feel free to report issues/suggestions.*

<img src="https://raw.githubusercontent.com/COMBINE-lab/mim/refs/heads/main/assets/mim.png" width=50% height=50% class="center" alt="Mímir">

Why `mim`? The project's name is a reference to the Norse figure [Mímir](https://en.wikipedia.org/wiki/M%C3%ADmir), who is:

> renowned for his knowledge and wisdom, who is beheaded during the Æsir–Vanir War. Afterward, the god Odin carries around Mímir's head and it recites secret knowledge and counsel to him.

the `mim` index is a small index that gives critical knowledge into the internal structure of a gzipped FASTA/Q file that allows rapid and efficient parallel parsing and decompression.

The purpose of `mim` is so that one can create a `mim` index for gzipped FASTQ files that they anticipate will be reprocessed more than once (e.g. either by themselves or by another party after being deposited in a public database like ENA or SRA).  Having the `mim` index available make subsequent parsing of the data *much* faster, enabling more rapid re-analysis of data (e.g. when new versions of tools or even entirely different analysis algorithms become available).

The `mim` index is purely additive (i.e. creating it does not modify or rewrite any part of the original file), small (typically about 1/1000-th the size of the compressed input file), and takes about as much time to make as simply parsing the input.  This makes it easy to create, store, transfer and share `mim` indexes.

Citation:

> mim: A lightweight auxiliary index to enable fast, parallel, gzipped FASTQ parsing.
> Rob Patro, Siddhant Bharti, Prajwal Singhania, Rakrish Dhakal, Thomas J. Dahlstrom, Ragnar Groot Koerkamp.
> https://doi.org/10.1101/2025.11.24.690271


## CLI usage

Install the `mim` binary using `cargo install mim-index`.
We require Rust version (MSRV) at least 1.91. If your Rust version is older than this, please upgrade by running `rustup update`.

The examples below assume an input file `records.fastq.gz`. `.fasta.gz` files
are also supported.

### Overview of subcommands
- `build`: build the `.mim` index file from a `.fastx.gz`.
- `unzip`: use the `.mim` index for parallel decompression into parts or pipes.
- `server`: run a server that stores a content-addressed-storage of `mim` files
  keyed by hashes of `gz` files.
- `upload`: upload a `.mim` to a server.
- `download`: download the `.mim` for a `.gz` from the server.

<details>
<summary>mim --help</summary>

```text
Simple program to deal with mim files

Usage: mim <COMMAND>

Commands:
  build     Build the .mim index

  unzip     Parallel-unzip a .fastx.gz using the .mim index

  info      Print .mim file metadata
  peek      print some reads
  nuc-hist  print some reads

  server    Run the server
  upload    Upload a mim file
  download  Download a file

  help      Print this message or the help of the given subcommand(s)
```
</details>

### Build the index: `mim build`
Build a `.fastq.gz.mim` index file for a given `.fastq.gz` file.

```sh
# Write records.fastq.gz.mim:
mim build records.fastq.gz
# Write in custom location:
mim build records.fastq.gz -m /path/to/records.fastq.gz.mim
# Use a chunk-size of 1GB instead of the default 32MB
mim build records.fastq.gz --chunck-size 1000000000
# Add custom json-encoded metadata.
mim build 
```

<details>
<summary>mim build --help</summary>

``` text
Build the .mim index

Usage: mim build [OPTIONS] <FASTX_GZ>

Arguments:
  <FASTX_GZ>  Input .fastx.gz file

Options:
  -m, --mim <INDEX_PATH>         .mim file to write; default <FASTX_GZ>.mim
  -c, --chunk-size <CHUNK_SIZE>  Distance between checkpoints [default: 32000000]
  -d, --metadata <METADATA>      Optional metadata to add. Json-encoded string
```
</details>


### Decompress using the index: `mim unzip`
Given a `records.fastq.gz` and `.fastq.gz.mim`, unzip using multiple threads into either:
- the plain `records.fastq`: `mim unzip records.fastq.gz`
- multiple `records.fastq.<ID>` file parts: `mim unzip records.fastq.gz --parts 8`
- multiple `records.fastq.<ID>` _named piped_: `mim unzip records.fastq.gz --parts 8 --pipe`
  This will block until all pipes have been read to completion by some other program.

<details>
<summary>mim unzip --help</summary>

``` text
Parallel-unzip a .fastx.gz using the .mim index

Usage: mim unzip [OPTIONS] <FASTX_GZ>

Arguments:
  <FASTX_GZ>  Input .fastx.gz file

Options:
  -m, --mim <INDEX_PATH>   .mim file to use; default <FASTX_GZ>.mim
  -o, --output <OUTPUT>    Output path. .fastx or .fastx.<part_id> by default
  -p, --parts <PARTS>      The number of .fastx.<part_id> parts to write
  -j, --threads <THREADS>  Number of threads to use. Defaults to number of cores
      --pipe               Fork and create a named pipe instead of file for each part. Requires --parts
```
</details>

### Unix socket server: `mim server`

The `.mim` index stores a Blake3 hash of the corresponding `.gz` file that is
verified before decompressing it. This hash is also used to globally identify
`.gz`. With `mim server`, one can launch a long-running binary that reads all
`.mim` files in a local directory and serves them over a unix socket, keyed by
the hash.

Clients can then use `mim upload records.fastq.gz.mim` to upload the `.mim` for a local `.gz` file to
the server, so that others can later use `mim download records.fastq.gz` to
download the mim based on the hash of `records.fastq.gz`.

This needs two arguments: the path where it will create a unix socket to listen
to incoming requests, and the directory where (previously) uploaded `.mim` files
are hosted.

``` sh
mim server --socket /tmp/mim-server --dir /mnt/data/mim-files
```

<details>
<summary>mim server --help</summary>

``` text
Run the server

Usage: mim server --socket <SOCKET> --dir <DIR>

Options:
      --socket <SOCKET>  Path to unix socket to listen on
      --dir <DIR>        The directory containing the .mim files
```
</details>

### Upload a binary: `mim upload`

To upload a local `.mim` file, use:
`mim upload --socket /tmp/mim-server records.fastq.gz.mim`

<details>
<summary>mim upload --help</summary>

``` text
Upload a mim file

Usage: mim upload --socket <SOCKET> <MIM>

Arguments:
  <MIM>  The .mim file to upload

Options:
      --socket <SOCKET>  Path to unix socket to connect to
```
</details>

### Download a binary: `mim download`
To download a `.mim` file for a local `.gz` file, run:
`mim download --socket /tmp/mim-server records.fastq.gz`.
Write it to a custom location using `-m custom/records.fastq.gz`.

<details>
<summary>mim download --help</summary>

``` text
Download a file

Usage: mim download [OPTIONS] --socket <SOCKET> <FASTX_GZ>

Arguments:
  <FASTX_GZ>  The .fastx.gz file to hash and download the .mim for

Options:
  -m, --mim <INDEX_PATH>  Output location of the .mim. Default <FASTX_GZ>.mim
      --socket <SOCKET>   Path to unix socket to connect to
```
</details>

## API usage

Given an existing `.mim` file, multi-threaded file parsing works by instantiating
a `MimReader` with the number of workers, and then calling `.readers()` to
return a reader for each thread. Each reader returns a record-aligned range of byte.

``` rust
use mim::MimReader;
let num_workers = 8;
// Requires `reords.fastq.gz.mim` to exist.
let reader: MimReader = mim::mim_reader(PathBuf::new("records.fastq.gz"), num_workers);
std::thread::scope(|s| {
    for reader in reader.readers() {
        let reader = reader.unwrap();
        s.spawn(|| {
            // Do something with the reader, e.g. read records from it.
        });
    }
});
```

We also provide `MimReader::get_needletail_parser` and
`MimReader::get_needletail_iter` that directly wrap the reader in a Needletail
parser and iterator over records.

See <src/lib.rs> or docs.rs for details.


#### Note: `mim` started originally as a class project for CMSC701 at the University of Maryland. 

The original approach, which has been altered substantially, was implemented for a final project in the Spring 2025 
edition of CMSC701 at UMD. The original implementation, from which this project eventually evolved, is available 
[here](https://github.com/siddhant-bharti/CMSC701-Project).

