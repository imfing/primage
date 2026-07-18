mod cli;
mod codecs;
mod pipeline;
mod transform;

use std::collections::HashSet;

use clap::Parser;
use rayon::prelude::*;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();

    // Phase 1 (serial): resolve output formats/paths and deduplicate names.
    let mut taken = HashSet::new();
    let mut plans = Vec::with_capacity(args.inputs.len());
    let mut failures = 0u32;
    for input in &args.inputs {
        match pipeline::plan(input, &args, &mut taken) {
            Ok(plan) => plans.push(plan),
            Err(e) => {
                eprintln!("{}: error: {e:#}", input.display());
                failures += 1;
            }
        }
    }

    // Phase 2 (parallel): decode, transform, encode, write.
    let results: Vec<_> = plans
        .par_iter()
        .map(|plan| pipeline::run(plan, &args))
        .collect();

    let mut totals = (0u64, 0u64);
    for (plan, result) in plans.iter().zip(results) {
        match result {
            Ok(report) => {
                totals.0 += report.input_size;
                totals.1 += report.output_size;
                println!("{report}");
            }
            Err(e) => {
                eprintln!("{}: error: {e:#}", plan.input.display());
                failures += 1;
            }
        }
    }

    if args.inputs.len() > 1 {
        let done = args.inputs.len() as u32 - failures;
        println!(
            "{done} file(s): {} → {}",
            pipeline::human_bytes(totals.0),
            pipeline::human_bytes(totals.1),
        );
    }
    if failures > 0 {
        anyhow::bail!("{failures} file(s) failed");
    }
    Ok(())
}
