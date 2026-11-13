//! Paired, alternating-order driver for 's profiling-disabled binaries.
//! Usage: layout_compare BASELINE NARROW COLD OUT (OUT must not exist).

use std::fs;
use std::path::Path;
use std::process::Command;

fn sample(binary: &str, role: &str, connections: usize, out: &Path) -> f64 {
    let output = Command::new(binary)
        .args([role, &connections.to_string(), "40000"])
        .output()
        .unwrap();
    fs::write(out.with_extension("stdout"), &output.stdout).unwrap();
    fs::write(out.with_extension("stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{}: {:?}",
        out.display(),
        output.status
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.lines().any(|s| s == "profiling,false"));
    assert!(
        stdout
            .lines()
            .any(|s| s.starts_with(&format!("verified,{role},")))
    );
    let rows: Vec<_> = stdout
        .lines()
        .filter(|s| s.starts_with("sample,"))
        .collect();
    assert_eq!(rows.len(), 1);
    rows[0].split(',').next_back().unwrap().parse().unwrap()
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(
        args.len(),
        4,
        "usage: layout_compare BASELINE NARROW COLD OUT"
    );
    let out = Path::new(&args[3]);
    fs::create_dir(out).unwrap();
    fs::write(out.join("commands.txt"), format!(
        "binaries: {:?}\nroles: sender, receiver\nconnections: 1, 64\nturns: 40000\npairs: 9\norder: baseline,candidate on even pairs; candidate,baseline on odd pairs\n",
        &args[..3])).unwrap();
    println!("candidate,role,connections,pair,baseline_ns,candidate_ns,change_percent");
    for (index, name) in ["control", "narrow", "cold"].iter().enumerate() {
        for role in ["sender", "receiver"] {
            for connections in [1, 64] {
                let mut changes = Vec::new();
                let mut baselines = Vec::new();
                let mut candidates = Vec::new();
                for pair in 0..9 {
                    let mut times = [0.0; 2];
                    let order = if pair % 2 == 0 { [0, 1] } else { [1, 0] };
                    for which in order {
                        let binary = if which == 0 { &args[0] } else { &args[index] };
                        let file = out.join(format!("{name}-{role}-{connections}-{pair}-{which}"));
                        times[which] = sample(binary, role, connections, &file);
                    }
                    let change = 100.0 * (times[1] / times[0] - 1.0);
                    println!(
                        "{name},{role},{connections},{pair},{},{},{change:.4}",
                        times[0], times[1]
                    );
                    changes.push(change);
                    baselines.push(times[0]);
                    candidates.push(times[1]);
                }
                let faster = changes.iter().filter(|&&x| x < 0.0).count();
                let middle = median(&mut changes);
                let baseline_median = median(&mut baselines);
                let candidate_median = median(&mut candidates);
                eprintln!(
                    "summary,{name},{role},{connections},paired-median={middle:.4}%,range={:.4}..{:.4}%,faster={faster}/9,baseline-ns={baseline_median},candidate-ns={candidate_median},baseline-range={:.0}..{:.0},candidate-range={:.0}..{:.0}",
                    changes[0],
                    changes[8],
                    baselines[0],
                    baselines[8],
                    candidates[0],
                    candidates[8]
                );
            }
        }
    }
}
