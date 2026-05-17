use anyhow::Result;
use clap::Parser;
use groundtruth_validator_cli::{
    args::{Args, OutputFormat},
    output::{print_json, print_summary},
    runner::run,
};

fn main() -> Result<()> {
    let args = Args::parse();

    // Honor `NO_COLOR` env var on top of the explicit flag.
    let use_color = !args.no_color && std::env::var_os("NO_COLOR").is_none();

    let reports = run(&args)?;

    match args.format {
        OutputFormat::Summary => print_summary(&reports, use_color, false),
        OutputFormat::Full => print_summary(&reports, use_color, true),
        OutputFormat::Json => print_json(&reports)?,
    }
    Ok(())
}
