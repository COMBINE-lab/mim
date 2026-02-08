//! Count the number of FASTA or FASTQ records in a stream of bytes.

#[derive(PartialEq, Eq)]
enum Type {
    Unknown,
    Fasta,
    Fastq,
}

pub struct RecordCounter {
    /// Type of records we're counting.
    /// Set on first push.
    tp: Type,

    /// Total number of bytes pushed so far.
    bytes_pushed: usize,

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
    pub fn new() -> Self {
        RecordCounter {
            tp: Type::Unknown,
            bytes_pushed: 0,
            records_started: 0,
            newlines: 0,
            last_newline: true,
        }
    }
    /// Push the given data, and return the total number of records started so far and the *global* byte offset of the first record starting in `data`.
    pub fn push_bytes(&mut self, data: &[u8]) -> (usize, Option<usize>) {
        let mut first_record_offset = None;

        if data.is_empty() {
            return (self.records_started, first_record_offset);
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
                        first_record_offset.get_or_insert(self.bytes_pushed + i);
                    }
                }
            }
            Type::Fastq => {
                if self.last_newline && self.newlines % 4 == 0 {
                    self.records_started += 1;
                    first_record_offset = Some(self.bytes_pushed);
                }
                for i in memchr::memchr_iter(b'\n', data) {
                    self.newlines += 1;
                    if self.newlines % 4 == 0 && i + 1 < data.len() {
                        self.records_started += 1;
                        first_record_offset.get_or_insert(self.bytes_pushed + i + 1);
                    }
                }
            }
            Type::Unknown => unreachable!(),
        }

        self.last_newline = data[data.len() - 1] == b'\n';
        self.bytes_pushed += data.len();
        (self.records_started, first_record_offset)
    }
}
