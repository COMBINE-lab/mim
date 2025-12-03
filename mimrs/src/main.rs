use clap::{Args, Parser, Subcommand};
use mimrs::gzip_reader::GzipStreamReader;
use mimrs::mim_types::deflate_index_load_gzip;
use mimrs::multi_parser::MultiParser;
use std::sync::{Arc, Mutex};

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::thread::JoinHandle;

#[derive(Subcommand, Debug)]
enum Commands {
    /// look insize an index
    Inspect(InspectCommand),
    /// print some reads
    Peek(PeekCommand),
    /// print some reads
    NucHist(NucHistCommand),
}

#[derive(Args, Debug)]
struct InspectCommand {
    /// path to index
    pub index_path: PathBuf,
}

#[derive(Args, Debug)]
struct PeekCommand {
    /// path to fastq
    pub fastq_path: PathBuf,

    /// path to index
    pub index_path: PathBuf,

    /// checkpoint to start at
    pub checkpoint: usize,

    /// number of reads to print
    pub nreads: usize,
}

#[derive(Args, Debug)]
struct NucHistCommand {
    /// path to fastq
    pub fastq_path: PathBuf,

    /// path to index
    pub index_path: PathBuf,

    /// number of threads to use
    pub nthreads: usize,
}

/// Simple program to deal with mim files
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    commands: Commands,
}

fn nuc_hist(args: &NucHistCommand) -> anyhow::Result<()> {
    let mut mp = Arc::new(MultiParser::new_with_workers(
        &args.fastq_path,
        &args.index_path,
        args.nthreads,
    ));

    let mut threads = Vec::<JoinHandle<Vec<usize>>>::with_capacity(args.nthreads);

    for t in 0..args.nthreads {
        let mp = mp.clone();
        threads.push(std::thread::spawn(move || {
            let mut wi = mp.get_worker_iter(t).expect("can get worker");
            let mut nucs = vec![0_usize; 4];
            eprintln!("starting parsing with thread {t}");
            while let Some(rec) = wi.next() {
                let record = rec.expect("valid record");
                record.seq().iter().for_each(|c| {
                    nucs[((*c as usize) >> 1) & 3] += 1;
                });
            }
            nucs
        }));
    }

    let mut nuc_hist = vec![0_usize; 4];
    for t in threads {
        let loc_nuc = t.join().expect("valid join");
        for (i, c) in loc_nuc.iter().enumerate() {
            nuc_hist[i] += c;
        }
    }

    eprintln!(
        "A: {}, C: {}, G: {}, T (or N): {}",
        nuc_hist[0], nuc_hist[1], nuc_hist[2], nuc_hist[3]
    );
    Ok(())
}

fn inspect_index(args: &InspectCommand) -> anyhow::Result<()> {
    let file = File::open(&args.index_path).expect("File failed to open");
    let index = deflate_index_load_gzip(file).expect("failed to load index");
    println!("metadata = {:#?}", index.metadata_dict);
    println!("number of checkpoints = {}", index.have);
    Ok(())
}

fn peek(args: &PeekCommand) -> anyhow::Result<()> {
    let file = File::open(&args.index_path).expect("File failed to index");
    let index = deflate_index_load_gzip(file).expect("failed to load index");
    assert!(
        args.checkpoint < index.list.len(),
        "requested checkpoint {} >= number of checkpoints {}",
        args.checkpoint,
        index.list.len()
    );

    eprintln!("opening at checkpoint: {}", args.checkpoint);
    let mut gzfq = GzipStreamReader::open_at_checkpoint(&args.fastq_path, &index, args.checkpoint)
        .expect("valid gzip stream reader");

    let record_offset = index.record_boundaries[args.checkpoint].byte_offset;
    if record_offset > gzfq.uncompressed_offset() {
        // discard the requisite number of bytes
        let mut discard_buf = vec![0_u8; (record_offset - gzfq.uncompressed_offset()) as usize];
        gzfq.read_exact(&mut discard_buf)?;
    }

    let mut fastx_reader = needletail::parse_fastx_reader(gzfq).expect("invalid reader");
    let mut idx = 0;
    while let Some(r) = fastx_reader.next() {
        let record = r.expect("invalid record");
        println!(
            "@{}\n{}",
            std::str::from_utf8(record.id()).expect("failed to convert name"),
            std::str::from_utf8(&record.seq()).expect("failed to convert seq")
        );
        idx += 1;
        if idx > args.nreads {
            break;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::try_parse()?;
    match cli.commands {
        Commands::Inspect(ref inspect_args) => {
            inspect_index(inspect_args)?;
        }
        Commands::Peek(ref peek_args) => {
            peek(peek_args)?;
        }
        Commands::NucHist(ref hist_args) => {
            nuc_hist(hist_args)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn test_load_index() {
        // Example usage
        let file = File::open("index.gz").expect("Failed to open file");
        let index = deflate_index_load_gzip(file).expect("Failed to load index");

        println!("Loaded index with {} access points", index.have);
        println!("Total record count: {}", index.total_record_count);
        println!("Uncompressed length: {}", index.length);
    }
}
