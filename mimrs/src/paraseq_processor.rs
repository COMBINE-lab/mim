#![allow(unused_variables)]
use std::thread;

use super::reader::MimReader;

use paraseq::{
    Result,
    fastx::{self, GenericReader},
    prelude::ParallelReader,
};

use paraseq::parallel::ProcessError;

impl ParallelReader for MimReader {
    type Rf<'a> = paraseq::fastx::RefRecord<'a>;

    /// Process the records in the input file in parallel using the given [`paraseq::prelude::ParallelProcessor`].
    ///
    /// `num_threads` must equal the number of workers in the `MimReader`.
    fn process_parallel<T>(self, processor: &mut T, num_threads: usize) -> Result<()>
    where
        T: for<'a> paraseq::prelude::ParallelProcessor<Self::Rf<'a>>,
    {
        assert!(
            num_threads == self.num_workers,
            "Number of threads ({}) must match the number of workers ({}) in the MimReader.",
            num_threads,
            self.num_workers
        );
        thread::scope(|scope| -> paraseq::parallel::Result<()> {
            // Spawn worker threads
            let mut handles = Vec::new();
            for thread_id in 0..num_threads {
                let mut worker_processor = processor.clone();
                let mim_reader = &self;
                let handle = scope.spawn(move || -> paraseq::parallel::Result<()> {
                    let mut reader = mim_reader.get_paraseq_reader(thread_id).unwrap();
                    let mut record_set = reader.new_record_set();

                    worker_processor.set_thread_id(thread_id);

                    loop {
                        let s1 = reader.fill(&mut record_set);

                        if !s1? {
                            break;
                        }

                        let records = fastx::RecordSet::iter(&record_set);

                        for record in records {
                            worker_processor.process_record(record?)?;
                        }

                        worker_processor.on_batch_complete()?;
                    }
                    worker_processor.on_thread_complete()?;
                    Ok(())
                });

                handles.push(handle);
            }

            // Wait for worker threads
            for handle in handles {
                match handle.join() {
                    Ok(Ok(())) => (),
                    Ok(Err(e)) => return Err(e),
                    Err(_) => return Err(ProcessError::JoinError),
                }
            }

            Ok(())
        })?;

        Ok(())
    }

    fn process_parallel_paired<T>(self, _: Self, _: &mut T, _: usize) -> Result<()>
    where
        T: for<'a> paraseq::prelude::PairedParallelProcessor<Self::Rf<'a>>,
    {
        todo!()
    }
    fn process_parallel_interleaved<T>(self, _: &mut T, _: usize) -> Result<()>
    where
        T: for<'a> paraseq::prelude::PairedParallelProcessor<Self::Rf<'a>>,
    {
        todo!()
    }
    fn process_parallel_multi<T>(self, _: Vec<Self>, _: &mut T, _: usize) -> Result<()>
    where
        T: for<'a> paraseq::prelude::MultiParallelProcessor<Self::Rf<'a>>,
        Self: Sized,
    {
        todo!()
    }
    fn process_parallel_multi_interleaved<T>(self, _: usize, _: &mut T, _: usize) -> Result<()>
    where
        T: for<'a> paraseq::prelude::MultiParallelProcessor<Self::Rf<'a>>,
    {
        todo!()
    }
}
