use clap::{Args, Parser, Subcommand};
use mimrs::mim_types::{DeflateIndex, Point, RecordCheckpoint, deflate_index_load_gzip};
use std::fs::File;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
enum Commands {
    /// does testing things
    Inspect(InspectCommand),
}

#[derive(Args, Debug)]
struct InspectCommand {
    /// path to index
    pub index_path: PathBuf,
}

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    commands: Commands,
}

fn inspect_index(args: &InspectCommand) -> anyhow::Result<()> {
    let file = File::open(&args.index_path).expect("File failed to open");
    let index = deflate_index_load_gzip(file).expect("failed to load index");
    println!("metadata {:#?}", index.metadata_dict);
    println!("{}", index.have);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::try_parse()?;
    match cli.commands {
        Commands::Inspect(ref inspect_args) => {
            inspect_index(inspect_args)?;
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
