
f:= "~/git/eth/data/legionella.fastq.gz"

build:
    ninja -C builddir
index:
    ./builddir/mimindex build $d/legionella.fastq.gz
run-mim-12:
    time ./builddir/test_mim_parser 12 {{f}} {{f}}.mim 
run-mim-6:
    time ./builddir/test_mim_parser 6 {{f}} {{f}}.mim 
run-mim-1:
    time ./builddir/test_mim_parser 1 {{f}} {{f}}.mim 
run-serial:
    time ./builddir/test_serial_parser {{f}}

run: run-serial run-mim-1 run-mim-6 run-mim-12


vbq_index:
    # --archive mode is like 2x slower.
    # We should probably bench against the fastest version as a baseline
    # to take into account future optimizations?
    # bqtools encode -T6 --archive -o {{f}}.vbq {{f}}
    # bqtools encode -T6 --uncompressed -o {{f}}.vbq {{f}}
    bqtools encode -T6 -H -S4 -m vbq -o {{f}}.vbq {{f}}
vbq-1:
    cd evals/binseq-test && time cargo run -r -- -T1 {{f}}.vbq
vbq-6:
    cd evals/binseq-test && time cargo run -r -- -T6 {{f}}.vbq
vbq-12:
    cd evals/binseq-test && time cargo run -r -- -T12 {{f}}.vbq

run-vbq: vbq-1 vbq-6 vbq-12
