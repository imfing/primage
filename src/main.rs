mod cli;
mod codecs;
mod pipeline;
mod transform;

use std::collections::HashSet;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();

    let mut failures = 0u32;
    let mut totals = (0u64, 0u64);
    let mut taken = HashSet::new();
    for input in &args.inputs {
        match pipeline::process(input, &args, &mut taken) {
            Ok(report) => {
                totals.0 += report.input_size;
                totals.1 += report.output_size;
                println!("{report}");
            }
            Err(e) => {
                eprintln!("{}: error: {e:#}", input.display());
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
