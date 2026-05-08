use clap::Parser;
use flpdf::{check_reader, write_pdf, Pdf, Severity};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
struct Args {
    #[arg(long)]
    check: bool,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    let result = if args.check {
        run_check(args.input)
    } else {
        run_rewrite(args.input, args.output)
    };

    if let Err(error) = result {
        eprintln!("flpdf: {error}");
        std::process::exit(2);
    }
}

fn run_check(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(input)?;
    let report = check_reader(BufReader::new(file))?;
    for diagnostic in report.diagnostics.entries() {
        let label = match diagnostic.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        eprintln!("{label}: {}", diagnostic.message);
    }
    if report.valid {
        println!("PDF check succeeded");
        Ok(())
    } else {
        Err("PDF check failed".into())
    }
}

fn run_rewrite(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let file = File::open(input)?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let mut out = File::create(output)?;
    write_pdf(&mut pdf, &mut out)?;
    Ok(())
}
