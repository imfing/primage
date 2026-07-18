mod cli;
mod codecs;
mod pipeline;
mod term;
mod transform;

use std::collections::HashSet;

use clap::Parser;
use rayon::prelude::*;

fn main() -> anyhow::Result<()> {
    let mut args = cli::Cli::parse();

    if args.preview && !term::supports_kitty() {
        if args.preview_only() {
            anyhow::bail!(
                "--preview requires a terminal supporting the Kitty graphics protocol \
                 (e.g. Ghostty, kitty, WezTerm)"
            );
        }
        eprintln!(
            "warning: terminal doesn't appear to support the Kitty graphics protocol; \
                   skipping preview"
        );
        args.preview = false;
    }
    let display_config = if args.preview {
        Some(term::DisplayConfig::detect()?)
    } else {
        None
    };

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
        .map(|plan| {
            pipeline::run(
                plan,
                &args,
                display_config.map(|config| config.pixel_width()),
            )
        })
        .collect();

    let mut totals = (0u64, 0u64);
    let mut converted = 0u32;
    for (plan, result) in plans.iter().zip(results) {
        match result {
            Ok(report) => {
                totals.0 += report.input_size;
                if let Some(size) = report.output_size {
                    totals.1 += size;
                    converted += 1;
                }
                println!("{report}");
                if let Some(img) = &report.image {
                    if let Err(e) = term::display(img) {
                        eprintln!("{}: preview failed: {e:#}", plan.input.display());
                    }
                }
            }
            Err(e) => {
                eprintln!("{}: error: {e:#}", plan.input.display());
                failures += 1;
            }
        }
    }

    if args.inputs.len() > 1 && converted > 0 {
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
