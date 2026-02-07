//! Count the number of FASTA or FASTQ records in a stream of bytes.

#[derive(PartialEq, Eq)]
enum Type {
    Unknown,
    Fasta,
    Fastq,
}

struct RecordCounter {
    /// Type of records we're counting.
    /// Set on first push.
    tp: Type,

    /// Number of records started so far.
    /// A record is started as soon as the initial > or @ is seen.
    records_started: usize,

    // Internal state to count records.
    /// Total number of newlines so far, for FASTQ record starts.
    newlines: usize,
    /// True when the last push ended in a newline.
    last_newline: bool,
}

impl RecordCounter {
    fn new() -> Self {
        RecordCounter {
            tp: Type::Unknown,
            records_started: 0,
            newlines: 0,
            last_newline: true,
        }
    }
    /// Push the given data, and return the total number of records started so far.
    fn push_bytes(&mut self, data: &[u8]) -> usize {
        if data.is_empty() {
            return 0;
        }

        if self.tp == Type::Unknown {
            self.tp = match data[0] {
                b'>' => Type::Fasta,
                b'@' => Type::Fastq,
                b => {
                    panic!("Unknown first byte {b} of fastx file.");
                }
            }
        }

        match self.tp {
            Type::Fasta => {
                // Scan for the next > and check if it's preceded by a newline.
                for i in memchr::memchr_iter(b'>', data) {
                    if (i == 0 && self.last_newline) || (i > 0 && data[i - 1] == b'\n') {
                        self.records_started += 1;
                    }
                }
            }
            Type::Fastq => {
                if self.last_newline && self.newlines % 4 == 0 {
                    self.records_started += 1;
                }
                for i in memchr::memchr_iter(b'\n', &data[..data.len() - 1]) {
                    self.newlines += 1;
                    if self.newlines % 4 == 0 {
                        self.records_started += 1;
                    }
                }
            }
            Type::Unknown => unreachable!(),
        }

        self.last_newline = data[data.len() - 1] == b'\n';
        self.records_started
    }
}
