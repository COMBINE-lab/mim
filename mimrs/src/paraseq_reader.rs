#![allow(unused_variables)]
use std::{
    path::{Path, PathBuf},
    thread,
};

use super::multi_parser::MultiParser;

use paraseq::{
    fastx::{self, GenericReader},
    prelude::{ParallelProcessor, ParallelReader},
};

use paraseq::parallel::ProcessError;

pub struct MimReader {
    file: PathBuf,
    index: PathBuf,
}

impl MimReader {
    /// Assume the mim index is simply at `<path>.mim`.
    pub fn new(path: &Path) -> Self {
        MimReader {
            file: path.to_owned(),
            index: path.to_owned().with_added_extension("mim"),
        }
    }
    pub fn new_with_index(path: &Path, index: &Path) -> Self {
        MimReader {
            file: path.to_owned(),
            index: index.to_owned(),
        }
    }
}

impl ParallelReader for MimReader {
    type Rf<'a> = paraseq::fastx::RefRecord<'a>;

    fn process_parallel<T>(self, processor: &mut T, num_threads: usize) -> paraseq::Result<()>
    where
        T: for<'a> paraseq::prelude::ParallelProcessor<Self::Rf<'a>>,
    {
        let multi_parser = MultiParser::new_with_workers(self.file, self.index, num_threads);
        let mut readers: Vec<(usize, fastx::Reader<_>)> = (0..num_threads)
            .map(|id| {
                let (num_records, stream) = multi_parser.get_worker_stream(id).unwrap();
                (num_records, fastx::Reader::new(stream).unwrap())
            })
            .collect();
        process_truly_parallel(&mut readers, processor)
    }

    fn process_parallel_paired<T>(
        self,
        r2: Self,
        processor: &mut T,
        num_threads: usize,
    ) -> paraseq::Result<()>
    where
        T: for<'a> paraseq::prelude::PairedParallelProcessor<Self::Rf<'a>>,
    {
        todo!()
    }

    fn process_parallel_interleaved<T>(
        self,
        processor: &mut T,
        num_threads: usize,
    ) -> paraseq::Result<()>
    where
        T: for<'a> paraseq::prelude::PairedParallelProcessor<Self::Rf<'a>>,
    {
        todo!()
    }

    fn process_parallel_multi<T>(
        self,
        rest: Vec<Self>,
        processor: &mut T,
        num_threads: usize,
    ) -> paraseq::Result<()>
    where
        T: for<'a> paraseq::prelude::MultiParallelProcessor<Self::Rf<'a>>,
        Self: Sized,
    {
        todo!()
    }

    fn process_parallel_multi_interleaved<T>(
        self,
        arity: usize,
        processor: &mut T,
        num_threads: usize,
    ) -> paraseq::Result<()>
    where
        T: for<'a> paraseq::prelude::MultiParallelProcessor<Self::Rf<'a>>,
    {
        todo!()
    }
}

fn process_truly_parallel<S, T>(
    readers: &mut [(usize, S)],
    processor: &mut T,
) -> paraseq::parallel::Result<()>
where
    T: for<'a> ParallelProcessor<S::RefRecord<'a>>,
    for<'a> <S as GenericReader>::RefRecord<'a>: paraseq::Record,
    S: GenericReader<Error: Into<ProcessError>> + Send,
{
    thread::scope(|scope| -> paraseq::parallel::Result<()> {
        // Spawn worker threads
        let mut handles = Vec::new();
        for (thread_id, (num_records, reader)) in readers.iter_mut().enumerate() {
            let mut worker_processor = processor.clone();
            let mut record_set = reader.new_record_set();

            let handle = scope.spawn(move || -> paraseq::parallel::Result<()> {
                worker_processor.set_thread_id(thread_id);

                let mut i = 0;
                'lp: loop {
                    let s1 = reader.fill(&mut record_set);

                    if !s1.map_err(Into::into)? {
                        break;
                    }

                    let records = S::iter(&record_set);

                    for record in records {
                        worker_processor.process_record(record.map_err(Into::into)?)?;
                        i += 1;
                        if i == *num_records {
                            worker_processor.on_batch_complete()?;
                            break 'lp;
                        }
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
