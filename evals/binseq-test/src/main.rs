//! Quick benchmark to count nucleotides using a .vbq input file.
//! Initial version written by Noam Teyssier @ Arc Institute.
use std::sync::Arc;

use anyhow::Result;
use binseq::{
    BinseqReader, ParallelProcessor as BinseqParallelProcessor,
    ParallelReader as BinseqParallelReader,
};
use clap::Parser;
use paraseq::{
    Record, fastx,
    parallel::ParallelProcessor as ParaseqParallelProcessor,
    prelude::{PairedParallelProcessor, ParallelReader as ParaseqParallelReader},
};
use parking_lot::Mutex;

type Counts = [u64; 4];

#[inline]
fn count_nucleotides(counts: &mut Counts, buf: &[u8]) {
    buf.iter().for_each(|c| {
        counts[((*c as usize) >> 1) & 3] += 1;
    });
}

#[derive(Clone, Default)]
pub struct Counter {
    sbuf: Vec<u8>,
    xbuf: Vec<u8>,
    local_counts: Counts,
    global_counts: Arc<Mutex<Counts>>,
}
impl Counter {
    pub fn clear_buffers(&mut self) {
        self.sbuf.clear();
        self.xbuf.clear();
    }
    pub fn merge_local_into_global(&mut self) {
        let mut global = self.global_counts.lock();
        for i in 0..4 {
            global[i] += self.local_counts[i];
        }
    }
    pub fn get_global_counts(&self) -> Counts {
        self.global_counts.lock().clone()
    }
    pub fn print_counts(&self) {
        let counts = self.get_global_counts();
        println!("Counts:");
        for i in 0..4 {
            println!("{i}: {}", counts[i]);
        }
    }
}
impl BinseqParallelProcessor for Counter {
    fn process_record<R: binseq::BinseqRecord>(&mut self, record: R) -> binseq::Result<()> {
        self.clear_buffers();
        record.decode_s(&mut self.sbuf)?;
        count_nucleotides(&mut self.local_counts, &self.sbuf);
        if record.is_paired() {
            record.decode_x(&mut self.xbuf)?;
            count_nucleotides(&mut self.local_counts, &self.xbuf);
        }
        Ok(())
    }

    fn on_batch_complete(&mut self) -> binseq::Result<()> {
        self.merge_local_into_global();
        Ok(())
    }
}

impl<R: Record> ParaseqParallelProcessor<R> for Counter {
    fn process_record(&mut self, record: R) -> paraseq::Result<()> {
        count_nucleotides(&mut self.local_counts, &record.seq());
        Ok(())
    }

    fn on_batch_complete(&mut self) -> paraseq::Result<()> {
        self.merge_local_into_global();
        Ok(())
    }
}

impl<R: Record> PairedParallelProcessor<R> for Counter {
    fn process_record_pair(&mut self, record1: R, record2: R) -> paraseq::Result<()> {
        count_nucleotides(&mut self.local_counts, &record1.seq());
        count_nucleotides(&mut self.local_counts, &record2.seq());
        Ok(())
    }

    fn on_batch_complete(&mut self) -> paraseq::Result<()> {
        self.merge_local_into_global();
        Ok(())
    }
}

#[derive(Parser)]
pub struct Cli {
    #[clap(num_args=1..2, required=true)]
    pub input: Vec<String>,

    #[clap(short = 'T', long, default_value_t = 0)]
    threads: usize,
}
impl Cli {
    pub fn is_paired(&self) -> bool {
        self.input.len() == 2
    }
    pub fn is_binseq(&self) -> bool {
        !self.is_paired() && (self.input[0].ends_with(".bq") || self.input[0].ends_with(".vbq"))
    }
    pub fn single_path(&self) -> &str {
        &self.input[0]
    }
    pub fn paired_path(&self) -> (&str, &str) {
        (&self.input[0], &self.input[1])
    }
    pub fn threads(&self) -> usize {
        if self.threads == 0 {
            num_cpus::get()
        } else {
            self.threads.min(num_cpus::get())
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut proc = Counter::default();
    if cli.is_binseq() {
        let path = cli.single_path();
        let reader = BinseqReader::new(path)?;
        reader.process_parallel(proc.clone(), cli.threads())?;
    } else if cli.is_paired() {
        let (path1, path2) = cli.paired_path();
        let r1 = fastx::Reader::from_path(path1)?;
        let r2 = fastx::Reader::from_path(path2)?;
        r1.process_parallel_paired(r2, &mut proc, cli.threads())?;
    } else {
        let path = cli.single_path();
        let reader = fastx::Reader::from_path(path)?;
        reader.process_parallel(&mut proc, cli.threads())?;
    }
    proc.print_counts();

    Ok(())
}
