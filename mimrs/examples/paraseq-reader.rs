use std::path::PathBuf;

use anyhow::Result;

use clap::Parser;
use mim::paraseq_reader::ParallelMimReader;
use paraseq::parallel::ParallelReader;
use paraseq::prelude::*;
use parking_lot::Mutex;

#[derive(Clone)]
pub struct Processor<'c> {
    global_counts: &'c Mutex<[usize; 4]>,
    local_counts: [usize; 4],
}
impl<'c> Processor<'c> {
    pub fn new(counts: &'c Mutex<[usize; 4]>) -> Self {
        Self {
            local_counts: [0; 4],
            global_counts: counts,
        }
    }
}
impl<'c, Rf: Record> ParallelProcessor<Rf> for Processor<'c> {
    fn process_record(&mut self, record: Rf) -> paraseq::parallel::Result<()> {
        for &c in record.seq().iter() {
            self.local_counts[(c >> 1) as usize & 3] += 1;
        }
        Ok(())
    }

    fn on_thread_complete(&mut self) -> paraseq::parallel::Result<()> {
        let mut global_out = self.global_counts.lock();
        for i in 0..4 {
            global_out[i] += self.local_counts[i];
        }
        Ok(())
    }
}

#[derive(Parser)]
struct Cli {
    /// Input file path
    input_file: PathBuf,

    /// Number of threads to use for processing
    num_threads: usize,
}

fn main() -> Result<()> {
    let args = Cli::parse();
    let reader = ParallelMimReader::new(&args.input_file);

    let counts = Mutex::new([0; 4]);
    let mut proc = Processor::new(&counts);
    reader.process_parallel(&mut proc, args.num_threads)?;
    eprintln!("Counts: {:?}", counts.into_inner());
    Ok(())
}
