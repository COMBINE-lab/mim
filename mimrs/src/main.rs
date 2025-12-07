use ::lender::prelude::*;
use clap::{Args, Parser, Subcommand};
use mimrs::gzip_reader::GzipStreamReader;
use mimrs::indexer;
use mimrs::mim_types::deflate_index_load_gzip;
use mimrs::multi_parser::{MultiPairParser, MultiParser, ReadIter};
use std::io;
use std::sync::Arc;
use tracing_subscriber::{EnvFilter, filter::LevelFilter, fmt, prelude::*};

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
    /// print some reads
    Build(BuildCommand),
}

#[derive(Args, Debug)]
struct InspectCommand {
    /// path to index
    pub index_path: PathBuf,
}

#[derive(Args, Debug)]
struct BuildCommand {
    /// path to fastx gz input
    #[arg(short, long)]
    pub fastq_path: PathBuf,

    /// optional output path
    #[arg(short, long)]
    pub index_path: Option<PathBuf>,

    /// span
    #[arg(short, long, default_value_t = 32_000_000)]
    pub span: usize,

    /// optional metadata
    #[arg(short, long)]
    pub metadata: Option<String>,
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
    #[arg(short, use_value_delimiter = true, value_delimiter = ',')]
    pub fastq_paths: Vec<PathBuf>,

    /// path to index
    #[arg(short, use_value_delimiter = true, value_delimiter = ',')]
    pub index_paths: Vec<PathBuf>,

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

fn launch_paired_parser(args: &NucHistCommand) -> anyhow::Result<()> {
    let mp = Arc::new(MultiPairParser::new_with_workers(
        &args.fastq_paths,
        &args.index_paths,
        args.nthreads,
    )?);

    let start = std::time::Instant::now();

    let mut threads = Vec::<JoinHandle<Vec<usize>>>::with_capacity(args.nthreads);

    for t in 0..args.nthreads {
        let mp = mp.clone();
        threads.push(std::thread::spawn(move || {
            let (mut wp1, mut wp2) = mp
                .get_needletail_parsers_for_worker(t)
                .expect("could get parsers");
            let mut nucs = vec![0_usize; 4];
            while let (Some(rec), Some(rec2)) = (wp1.next(), wp2.next()) {
                let record = rec.expect("valid record");
                record.seq().iter().for_each(|c| {
                    nucs[((*c as usize) >> 1) & 3] += 1;
                });
                let record = rec2.expect("valid record");
                record.seq().iter().for_each(|c| {
                    nucs[((*c as usize) >> 1) & 3] += 1;
                });
            }
            nucs
        }));
    }

    let mut nuc_hist = [0_usize; 4];
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

    eprintln!("took: {:?}", start.elapsed());

    Ok(())
}

fn launch_single_parser(args: &NucHistCommand) -> anyhow::Result<()> {
    let mp = Arc::new(MultiParser::new_with_workers(
        &args.fastq_paths[0],
        &args.index_paths[0],
        args.nthreads,
    ));

    let start = std::time::Instant::now();

    let mut threads = Vec::<JoinHandle<Vec<usize>>>::with_capacity(args.nthreads);

    for t in 0..args.nthreads {
        let mp = mp.clone();
        threads.push(std::thread::spawn(move || {
            let mut wi = mp
                .get_needletail_parser_for_worker(t)
                .expect("can get worker");
            let mut nucs = vec![0_usize; 4];
            while let Some(rec) = wi.next() {
                let record = rec.expect("valid record");
                record.seq().iter().for_each(|c| {
                    nucs[((*c as usize) >> 1) & 3] += 1;
                });
            }
            nucs
        }));
    }

    let mut nuc_hist = [0_usize; 4];
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

    eprintln!("took: {:?}", start.elapsed());

    Ok(())
}

fn nuc_hist(args: &NucHistCommand) -> anyhow::Result<()> {
    assert_eq!(args.fastq_paths.len(), args.index_paths.len());
    assert!(args.fastq_paths.len() <= 2);

    if args.fastq_paths.len() == 1 {
        launch_single_parser(args)?
    } else {
        launch_paired_parser(args)?
    }
    Ok(())
}

fn build_index(args: &BuildCommand) -> anyhow::Result<()> {
    let user_metadata = args
        .metadata
        .clone()
        .map(|s| serde_json::from_str(&s).expect("metadata must be valid json"));
    indexer::build_index(
        &args.fastq_path,
        args.span as i64,
        user_metadata,
        args.index_path.as_ref(),
    )?;
    Ok(())
}

fn inspect_index(args: &InspectCommand) -> anyhow::Result<()> {
    let file = File::open(&args.index_path)?;
    let index = deflate_index_load_gzip(file)?;
    let metadata_dict: serde_cbor::Value =
        serde_cbor::from_slice(&index.metadata_dict).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("CBOR parse error: {}", e),
            )
        })?;

    println!("loaded mim index for : {:?}", args.index_path.as_path());
    println!("metadata = {:?}", &metadata_dict);
    println!(
        "blake3 checksum = {}",
        base16::encode_lower(&index.compressed_hash)
    );
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

    let fastx_reader = needletail::parse_fastx_reader(gzfq).expect("invalid reader");
    let mut ri = ReadIter::new(fastx_reader, args.nreads);
    while let Some(r) = ri.next() {
        let record = r.expect("invalid record");
        println!(
            "@{}\n{}",
            std::str::from_utf8(record.id()).expect("failed to convert name"),
            std::str::from_utf8(&record.seq()).expect("failed to convert seq")
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    let (filtered_layer, _reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

    // set up the logging.  Here we will take the
    // logging level from the environment variable if
    // it is set.  Otherwise, we'll set the default
    tracing_subscriber::registry()
        // log level to INFO.
        .with(fmt::layer().with_writer(io::stderr))
        .with(filtered_layer)
        .init();
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
        Commands::Build(ref build_args) => {
            build_index(build_args)?;
        }
    }
    Ok(())
}
