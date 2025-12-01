import os
import sys
import pypst

def get_seconds(ts):
    toks = ts.split(':')
    print(toks)
    tsec = 0.0
    mult = [3600, 60, 1]
    for i in range(-1, -len(toks)-1, -1):
        tsec += float(toks[i]) * mult[i]
    return tsec

def parse_verbose(ifile):
    for l in ifile:
        if l.strip().startswith('Elapsed'):
            t = l.split()[7]
            return get_seconds(t)

def parse_simple(ifile):
    l = ifile.readline().strip()
    t = l.split()[2].rstrip('elapsed')
    return get_seconds(t)


def main(timing_dir):
    import pandas as pd
    dat = []
    vbq_dir = "vbq"
    for tdir, dset, files in os.walk(timing_dir):
        td = tdir.split(os.path.sep)[-1]
        if td == "vbq": continue
        if len(files) == 0:
            continue
        else:
            for f in files:
                nts = f.rstrip('.time')
                if nts != "cons":
                    #print(nts)
                    nt = int(nts)
                else: 
                    nt = nts
                fname = os.path.sep.join([tdir, f])
                #print(f"{nt} :: {fname}")
                total_sec = 0.0
                with open(fname) as ifile:
                    if nt == 1 or nt == "cons" :
                        total_sec = parse_verbose(ifile)
                    else:
                        total_sec = parse_simple(ifile)
                dat.append((td, nt, total_sec, 'mim'))
    for tdir, dset, files in os.walk(timing_dir):
        if not tdir.endswith("vbq"): continue 
        td = tdir.split(os.path.sep)[-2]
        for f in files:
            nt = nts = int(f.rstrip('.time'))
            fname = os.path.sep.join([tdir, f])
            print(f"{nt} :: {fname}")
            total_sec = 0.0
            with open(fname) as ifile:
                if nt == 1 or nt == "cons" :
                    total_sec = parse_verbose(ifile)
                else:
                    print(f"PARSE SIMPLE {fname}, {nt}")
                    total_sec = parse_simple(ifile)
            dat.append((td, nt, total_sec, 'vbq'))

    print(dat)
    df = pd.DataFrame(dat, columns = ['dataset', 'threads', 'time(s)', 'method'])
    df.to_csv('timing_dat.csv')
    df = df.pivot(index='dataset', columns=['threads', 'method'])
    df = df.sort_values(by='dataset')
    # top-level (value column)
    val = df.columns.levels[0][0]    # 'time(s)'

    mim_threads = [1, 2, 4, 8, 12, 16, 20, 24, "cons"]
    vbq_threads = [1, 2, 4, 8, 12, 16, 20, 24]

    # Build 3-level tuples
    desired = (
        [(val, t, "mim") for t in mim_threads] +
        [(val, t, "vbq") for t in vbq_threads]
    )

    desired_cols = pd.MultiIndex.from_tuples(desired, names=df.columns.names)

    df = df.reindex(columns=desired_cols)
    #mim_threads  = [1, 2, 4, 8, 12, 16, 20, 24, "cons"]
    #vbq_threads  = [1, 2, 4, 8, 12, 16, 20, 24]

    #desired = (
    #    [(t, "mim") for t in mim_threads] +
    #    [(t, "vbq") for t in vbq_threads]
    #)

    #desired_cols = pd.MultiIndex.from_tuples(desired, names=df.columns.names)
    #df = df.reindex(columns=desired_cols)
    print(df)
    #print(df.to_latex())
    #print(df.to_markdown())
    #print(df.columns)
    #table = pypst.Table.from_dataframe(df.loc[:, ['dataset', 'const', '1', '2', '4', '8', '12', '16', '20', '24']])
    #print(table)


if __name__ == "__main__":
    main(sys.argv[1])
