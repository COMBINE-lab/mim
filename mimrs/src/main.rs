use mimrs::mim_types::{DeflateIndex, Point, RecordCheckpoint, deflate_index_load_gzip};
use std::env;
use std::fs::File;

fn main() {
    let args: Vec<String> = env::args().collect();
    let file = File::open(&args[1]).expect("File failed to open");
    let index = deflate_index_load_gzip(file).expect("failed to load index");
    println!("metadata {:#?}", index.metadata_dict);
    println!("{}", index.have);
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
