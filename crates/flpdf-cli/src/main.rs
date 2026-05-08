use clap::Parser;
use flpdf::{check_reader, write_pdf, Object, Pdf, Severity};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
struct Args {
    #[arg(long)]
    check: bool,
    #[arg(long)]
    dump_object: Option<String>,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    let result = if let Some(object_ref) = args.dump_object.as_deref() {
        run_dump_object(args.input, object_ref)
    } else if args.check {
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

fn run_dump_object(
    input: Option<PathBuf>,
    object_ref: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let object_ref = parse_object_ref(object_ref)?;

    let file = File::open(input)?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let object = pdf.resolve(object_ref)?;

    if matches!(object, Object::Null) {
        return Err(format!(
            "object {} {} R not found",
            object_ref.number, object_ref.generation
        )
        .into());
    }

    let mut out = Vec::new();
    object.write_pdf(&mut out);
    println!("{}", String::from_utf8_lossy(&out));
    Ok(())
}

fn parse_object_ref(raw: &str) -> Result<flpdf::ObjectRef, String> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 2 && parts.len() != 3 {
        return Err(format!("invalid object ref '{raw}'"));
    }

    if parts.len() == 3 && parts[2] != "R" {
        return Err(format!("invalid object ref '{raw}'"));
    }

    let number = parts[0]
        .parse::<u32>()
        .map_err(|_| format!("invalid object number in '{raw}'"))?;
    let generation = parts[1]
        .parse::<u16>()
        .map_err(|_| format!("invalid object generation in '{raw}'"))?;

    Ok(flpdf::ObjectRef::new(number, generation))
}
