use clap::{load_yaml, App, ArgMatches};

use d4::{
    task::{Histogram, Mean, SimpleTask, Task, TaskOutput},
    D4TrackReader,
};

use std::path::Path;
use std::{
    borrow::{Borrow, Cow},
    io::{BufRead, BufReader},
};
use std::{fs::File, iter::Once};

fn parse_bed_file<P: AsRef<Path>>(
    file: P,
) -> std::io::Result<impl Iterator<Item = (String, u32, u32)>> {
    let file = BufReader::new(File::open(file)?);
    Ok(file.lines().filter_map(|line| {
        if let Ok(line) = line {
            let tokenized: Vec<_> = line.split(|c| c == '\t').take(3).collect();
            if tokenized.len() == 3 {
                if let Ok(left) = tokenized[1].parse() {
                    if let Ok(right) = tokenized[2].parse() {
                        return Some((tokenized[0].to_owned(), left, right));
                    }
                }
            }
        }
        None
    }))
}

fn open_file_parse_region_and_then<T, F>(
    matches: ArgMatches,
    func: F,
) -> Result<T, Box<dyn std::error::Error>>
where
    F: FnOnce(Vec<D4TrackReader>, Vec<(String, u32, u32)>) -> Result<T, Box<dyn std::error::Error>>,
{
    let input_filename = matches.value_of("input").unwrap();
    let mut data_path = vec![];

    let d4files: Vec<D4TrackReader> = if matches.is_present("first") || input_filename.contains(':')
    {
        data_path.push("<default>".to_string());
        vec![D4TrackReader::open(input_filename)?]
    } else if let Some(pattern) = matches.value_of("filter") {
        let pattern = regex::Regex::new(pattern)?;
        D4TrackReader::open_tracks(input_filename, |path| {
            let stem = path
                .map(|what: &Path| {
                    what.file_name()
                        .map(|x| x.to_string_lossy())
                        .unwrap_or_else(|| Cow::<str>::Borrowed(""))
                })
                .unwrap_or_default();
            if pattern.is_match(stem.borrow()) {
                data_path.push(stem.to_string());
                true
            } else {
                false
            }
        })?
    } else {
        D4TrackReader::open_tracks(input_filename, |path| {
            let stem = path
                .map(|what: &Path| {
                    what.file_name()
                        .map(|x| x.to_string_lossy())
                        .unwrap_or_else(|| Cow::<str>::Borrowed(""))
                })
                .unwrap_or_default();
            data_path.push(stem.to_string());
            true
        })?
    };

    let region_spec: Vec<_> = if let Some(path) = matches.value_of("region") {
        parse_bed_file(path)?
            .map(|(chr, left, right)| (chr, left, right))
            .collect()
    } else {
        d4files[0]
            .header()
            .chrom_list()
            .iter()
            .map(|chrom| (chrom.name.clone(), 0u32, chrom.size as u32))
            .collect()
    };
    func(d4files, region_spec)
}

pub struct OwnedOutput<T> {
    chrom: String,
    begin: u32,
    end: u32,
    output: T,
}

#[allow(clippy::type_complexity)]
fn run_task<T: Task<Once<i32>> + SimpleTask + Clone>(
    matches: ArgMatches,
) -> Result<Vec<OwnedOutput<Vec<T::Output>>>, Box<dyn std::error::Error>>
where
    T::Output: Clone,
{
    open_file_parse_region_and_then(matches, |inputs, region_spec| {
        /*
          let tasks: Vec<_> = region_spec
                .iter()
                .map(|(chr, begin, end)| {
                    VectorStat::create_vector_task(inputs.len(), T::new(chr, *begin, *end))
                })
                .collect();
            let mut inputs = D4MatrixReader::new(inputs)?;
            Ok(TaskContext::new(&mut inputs, tasks)?.run())
        */
        let mut ret = vec![];
        for mut input in inputs {
            let result = T::create_task(&mut input, &region_spec)?.run();
            for (idx, result) in result.into_iter().enumerate() {
                if ret.len() <= idx {
                    ret.push(OwnedOutput {
                        output: vec![result.output.clone()],
                        begin: result.begin,
                        end: result.end,
                        chrom: result.chrom.to_string(),
                    });
                } else {
                    ret[idx].output.push(result.output.clone());
                }
            }
        }
        Ok(ret)
    })
}

fn percentile_stat(matches: ArgMatches, percentile: f64) -> Result<(), Box<dyn std::error::Error>> {
    let histograms = run_task::<Histogram>(matches)?;
    for OwnedOutput {
        chrom: chr,
        begin,
        end,
        output: results,
    } in histograms
    {
        print!("{}\t{}\t{}", chr, begin, end);
        for (below, hist, above) in results {
            let count: u32 = below + hist.iter().sum::<u32>() + above;
            let below_count = (count as f64 * percentile.min(1.0).max(0.0)).round() as u32;
            let mut current = below;
            let mut idx = 0;
            while current < below_count && (idx as usize) < hist.len() {
                current += hist[idx];
                idx += 1;
            }
            println!("\t{}", idx);
        }
        println!();
    }
    Ok(())
}

fn hist_stat(matches: ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let max_bin = matches.value_of("max-bin").unwrap_or("1000").parse()?;
    let histograms = open_file_parse_region_and_then(matches, |mut input, regions| {
        let tasks: Vec<_> = regions
            .into_iter()
            .map(|(chr, begin, end)| Histogram::with_bin_range(&chr, begin, end, 0..max_bin))
            .collect();
        Ok(Histogram::create_task(&mut input[0], tasks)?.run())
    })?;
    let mut hist_result = vec![0; max_bin as usize + 1];
    let (mut below, mut above) = (0, 0);
    for TaskOutput {
        output: (b, hist, a),
        ..
    } in histograms.into_iter()
    {
        below += b;
        above += a;
        for (id, val) in hist.iter().enumerate() {
            hist_result[id + 1] += val;
        }
    }

    println!("<0\t{}", below);
    for (val, cnt) in hist_result[1..].iter().enumerate() {
        println!("{}\t{}", val, cnt);
    }
    println!(">{}\t{}", max_bin, above);

    Ok(())
}

pub fn entry_point(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml)
        .version(d4::VERSION)
        .get_matches_from(&args);
    if let Some(threads) = matches.value_of("threads") {
        let threads = threads.parse().unwrap();
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }
    match matches.value_of("stat") {
        None | Some("mean") | Some("avg") => {
            for result in run_task::<Mean>(matches)? {
                print!("{}\t{}\t{}", result.chrom, result.begin, result.end);
                for value in result.output {
                    print!("\t{}", value)
                }
                println!();
            }
        }
        Some("median") => {
            percentile_stat(matches, 0.5)?;
        }
        Some("hist") => {
            hist_stat(matches)?;
        }
        Some(whatever) if whatever.starts_with("percentile=") => {
            let prefix_len = "percentile=".len();
            let percentile: f64 = whatever[prefix_len..].parse()?;
            percentile_stat(matches, percentile / 100.0)?;
        }
        _ => panic!("Unsupported stat type"),
    }
    Ok(())
}
