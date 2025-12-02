use clap::{Args, Parser, Subcommand};
use mimrs::gzip_reader::GzipStreamReader;
use mimrs::mim_types::deflate_index_load_gzip;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
enum Commands {
    /// look insize an index
    Inspect(InspectCommand),
    /// print some reads
    Peek(PeekCommand),
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

/// Simple program to deal with mim files
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    commands: Commands,
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
