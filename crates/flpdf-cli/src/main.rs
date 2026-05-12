use clap::{Args as ClapArgs, Parser, Subcommand};
use flpdf::{
    check_reader_with_options, fonts,
    linearization::{
        check_linearization_path, write_linearized, LinearizationCheckError, LinearizationPlan,
        RenumberMap,
    },
    outline, pages, parse_pdf_version, write_pdf_with_options, write_qdf, Object, ObjectRef, Pdf,
    PdfOpenOptions, Severity, WriteOptions,
};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // Legacy options kept for compatibility.
    #[arg(long)]
    check: bool,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    #[arg(long)]
    dump_object: Option<String>,
    #[arg(long)]
    show_info: bool,
    #[arg(long)]
    show_catalog: bool,
    #[arg(long)]
    show_metadata: bool,
    #[arg(long)]
    show_outline: bool,
    #[arg(long)]
    show_fonts: bool,
    #[arg(long)]
    show_npages: bool,
    #[arg(long)]
    show_pages: bool,

    // qpdf-style top-level write flags. When `--linearize` is set together
    // with INPUT and OUTPUT, behave as if `flpdf rewrite --linearize ...`
    // had been invoked. This exists so the qpdf qtest acceptance harness
    // (PATH-shimmed `qpdf` → `flpdf`) can issue qpdf-shaped commands
    // without an arg-translating wrapper.
    /// Produce a linearized ("fast web view") output PDF (top-level alias
    /// of `flpdf rewrite --linearize`).
    #[arg(long)]
    linearize: bool,
    /// Use a fixed value for the trailer /ID's changing identifier
    /// (top-level alias of `flpdf rewrite --static-id`). Testing only.
    #[arg(long = "static-id")]
    static_id: bool,
    /// `qpdf --compress-streams=y|n` compatibility flag.  Accepted but
    /// currently a no-op: flpdf does not re-encode stream contents on
    /// rewrite.  Provided so qtest commands parse cleanly.
    #[arg(long = "compress-streams")]
    compress_streams: Option<String>,
    /// `qpdf --linearize-pass1=PATH` compatibility flag.  Accepted; flpdf
    /// writes the pass-1 intermediate file as a copy of the final
    /// linearized output (qpdf writes a distinct intermediate; matching
    /// those bytes is out of scope here — see flpdf-vrn).
    #[arg(long = "linearize-pass1")]
    linearize_pass1: Option<PathBuf>,

    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Validate PDF structure and report diagnostics")]
    Check(CheckCommand),
    #[command(
        name = "check-linearization",
        about = "Validate linearization structure (param dict, hint stream, offsets)"
    )]
    CheckLinearization(CheckLinearizationCommand),
    #[command(name = "dump-object", about = "Dump one indirect object as PDF syntax")]
    DumpObject(DumpObjectCommand),
    #[command(about = "Show page structure summary or detail")]
    Pages(PagesCommand),
    #[command(about = "Write a qdf-style raw dump into a file")]
    Qdf(QdfCommand),
    #[command(about = "Rewrite the input PDF to a normalized output")]
    Rewrite(RewriteCommand),
}

#[derive(Debug, ClapArgs)]
struct CheckCommand {
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct CheckLinearizationCommand {
    /// Input PDF file to validate.
    input: PathBuf,
}

#[derive(Debug, ClapArgs)]
struct DumpObjectCommand {
    object_ref: String,
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct PagesCommand {
    input: PathBuf,
    #[arg(long)]
    count: bool,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct QdfCommand {
    input: PathBuf,
    output: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct RewriteCommand {
    input: PathBuf,
    output: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    /// Produce a linearized ("fast web view") output PDF.
    #[arg(long)]
    linearize: bool,
    /// Use a fixed value for the trailer /ID's changing identifier (qpdf
    /// --static-id equivalent). Testing only; not for production output.
    #[arg(long = "static-id")]
    static_id: bool,
    /// Set a minimum PDF version for the output header.
    ///
    /// The effective version is `max(source_version, min_version)`.
    /// Mirrors `qpdf --min-version`.
    #[arg(long = "min-version")]
    min_version: Option<String>,
    /// Force the output PDF version header to exactly this value.
    ///
    /// Overrides source version and the linearize 1.2 floor.
    /// Mirrors `qpdf --force-version`.
    #[arg(long = "force-version")]
    force_version: Option<String>,
    /// Decode every stream through its filter chain and re-emit the document
    /// end-to-end with a single /FlateDecode filter per stream.  The output
    /// contains no /Prev chain.  Cannot be combined with --linearize.
    #[arg(long = "full-rewrite")]
    full_rewrite: bool,
}

#[derive(Debug, Clone, Default, ClapArgs)]
struct PasswordArgs {
    /// Password bytes for encrypted PDFs.
    #[arg(long, conflicts_with = "password_file")]
    password: Option<String>,
    /// File containing password bytes. One trailing LF or CRLF is stripped.
    #[arg(long = "password-file", value_name = "PATH")]
    password_file: Option<PathBuf>,
}

fn main() {
    let args = Cli::parse();

    let result = if let Some(command) = args.command {
        run_command(command)
    } else if let Some(object_ref) = args.dump_object.as_deref() {
        run_dump_object(args.input, args.repair, &args.password, object_ref)
    } else if args.show_info {
        run_show_info(args.input, args.repair, &args.password)
    } else if args.show_catalog {
        run_show_catalog(args.input, args.repair, &args.password)
    } else if args.show_metadata {
        run_show_metadata(args.input, args.repair, &args.password)
    } else if args.show_outline {
        run_show_outline(args.input, args.repair, &args.password)
    } else if args.show_fonts {
        run_show_fonts(args.input, args.repair, &args.password)
    } else if args.show_npages {
        run_show_npages(args.input, args.repair, &args.password)
    } else if args.show_pages {
        run_show_pages(args.input, args.repair, &args.password)
    } else if args.check {
        run_check(args.input, args.repair, &args.password)
    } else if args.linearize {
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
        let result = run_rewrite(
            args.input,
            args.output.clone(),
            args.repair,
            &args.password,
            true,
            options,
        );
        if result.is_ok() {
            if let (Some(pass1), Some(output)) =
                (args.linearize_pass1.as_ref(), args.output.as_ref())
            {
                // qpdf --linearize-pass1=PATH dumps the pre-back-patched pass1
                // intermediate. flpdf does not currently expose that internal
                // state; copy the final output instead so the path exists and
                // downstream byte-equality checks fail meaningfully rather
                // than the file being absent.
                if let Err(error) = std::fs::copy(output, pass1) {
                    eprintln!("flpdf: failed to write --linearize-pass1 file: {error}");
                    std::process::exit(2);
                }
            }
        }
        result
    } else {
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
        run_rewrite(
            args.input,
            args.output,
            args.repair,
            &args.password,
            false,
            options,
        )
    };

    if let Err(error) = result {
        eprintln!("flpdf: {error}");
        std::process::exit(2);
    }
}

fn run_command(command: Commands) -> CliResult<()> {
    match command {
        Commands::Check(cmd) => run_check(Some(cmd.input), cmd.repair, &cmd.password),
        Commands::CheckLinearization(cmd) => match check_linearization_path(&cmd.input) {
            Ok(()) => {
                println!("linearization OK");
                Ok(())
            }
            Err(LinearizationCheckError::NotLinearized) => {
                eprintln!(
                    "flpdf: not a linearized PDF: the first object in the file has no /Linearized key"
                );
                std::process::exit(1);
            }
            Err(LinearizationCheckError::InvalidParam { message }) => {
                eprintln!("flpdf: linearization check failed: {message}");
                std::process::exit(1);
            }
            Err(LinearizationCheckError::Io(e)) => Err(e.to_string().into()),
        },
        Commands::DumpObject(cmd) => {
            run_dump_object(Some(cmd.input), cmd.repair, &cmd.password, &cmd.object_ref)
        }
        Commands::Pages(cmd) => {
            if cmd.count {
                run_show_npages(Some(cmd.input), cmd.repair, &cmd.password)
            } else {
                run_show_pages(Some(cmd.input), cmd.repair, &cmd.password)
            }
        }
        Commands::Qdf(cmd) => run_qdf(Some(cmd.input), Some(cmd.output), cmd.repair, &cmd.password),
        Commands::Rewrite(cmd) => {
            if let Some(ref v) = cmd.force_version {
                if parse_pdf_version(v).is_none() {
                    eprintln!("flpdf: invalid --force-version value: {:?}", v);
                    std::process::exit(1);
                }
            }
            if let Some(ref v) = cmd.min_version {
                if parse_pdf_version(v).is_none() {
                    eprintln!("flpdf: invalid --min-version value: {:?}", v);
                    std::process::exit(1);
                }
            }
            if cmd.full_rewrite && cmd.linearize {
                eprintln!("flpdf: --full-rewrite and --linearize cannot be used together");
                std::process::exit(1);
            }
            let mut options = WriteOptions::default();
            options.static_id = cmd.static_id;
            options.min_version = cmd.min_version;
            options.force_version = cmd.force_version;
            options.full_rewrite = cmd.full_rewrite;
            run_rewrite(
                Some(cmd.input),
                Some(cmd.output),
                cmd.repair,
                &cmd.password,
                cmd.linearize,
                options,
            )
        }
    }
}

fn run_check(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(input)?;
    let options = pdf_open_options(repair, password)?;
    let report = check_reader_with_options(BufReader::new(file), options)
        .map_err(actionable_password_error)?;
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
    repair: bool,
    password: &PasswordArgs,
    linearize: bool,
    options: WriteOptions,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;

    if linearize {
        let mut pdf = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf)?;
        let plan = LinearizationPlan::from_pdf(&mut pdf)?;
        let renumber = RenumberMap::from_plan(&plan);

        // Re-open the PDF so `write_linearized` can seek/read objects independently.
        let mut pdf2 = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf2)?;
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &options)?;
        doc.back_patch()?;

        std::fs::write(&output, &doc.bytes)?;
    } else {
        let mut pdf = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf)?;
        let mut out = File::create(output)?;
        write_pdf_with_options(&mut pdf, &mut out, &options)?;
    }
    Ok(())
}

fn run_qdf(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    reject_encrypted_write(&pdf)?;

    let mut out = File::create(output)?;
    write_qdf(&mut pdf, &mut out)?;
    Ok(())
}

fn reject_encrypted_write<R: std::io::Read + std::io::Seek>(pdf: &Pdf<R>) -> CliResult<()> {
    if pdf.is_encrypted() {
        return Err("encrypted PDF output is not supported yet; decrypt/re-encrypt support is tracked separately".into());
    }
    Ok(())
}

fn run_dump_object(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    object_ref: &str,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let object_ref = ObjectRef::parse(object_ref)?;

    let mut pdf = open_pdf(&input, repair, password)?;
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

fn run_show_info(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let info_ref = pdf
        .trailer()
        .get_ref("Info")
        .ok_or("document info dictionary not found")?;
    let info = pdf.resolve(info_ref)?;

    let Object::Dictionary(dict) = info else {
        return Err(format!("info object {} is not a dictionary", info_ref).into());
    };

    println!("Info:");
    for (key, value) in dict.iter() {
        let key = String::from_utf8_lossy(key);
        println!("  {} = {}", key, object_to_pdf(value));
    }
    Ok(())
}

fn run_show_catalog(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve(catalog_ref)?;
    println!("Catalog: {}", object_to_pdf(&catalog));
    Ok(())
}

fn run_show_metadata(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve(catalog_ref)?;

    let Object::Dictionary(catalog) = catalog else {
        return Err(format!("document catalog {} is not a dictionary", catalog_ref).into());
    };

    match catalog.get_ref("Metadata") {
        Some(metadata_ref) => {
            let metadata = pdf.resolve(metadata_ref)?;
            match metadata {
                Object::Stream(stream) => {
                    let kind = stream
                        .dict
                        .get("Subtype")
                        .map(object_to_pdf)
                        .unwrap_or_else(|| String::from("Unknown"));
                    println!("Metadata: stream ({}) {}", metadata_ref, kind);
                    println!("  length: {}", stream.data.len());
                    const MAX_METADATA_PREVIEW_BYTES: usize = 1000;
                    let preview_len = stream.data.len().min(MAX_METADATA_PREVIEW_BYTES);
                    let preview = String::from_utf8_lossy(&stream.data[..preview_len]);
                    if stream.data.len() > MAX_METADATA_PREVIEW_BYTES {
                        println!(
                            "  preview: {} (truncated, total {} bytes)",
                            preview,
                            stream.data.len()
                        );
                    } else {
                        println!("  preview: {}", preview);
                    }
                }
                Object::Dictionary(dict) => {
                    println!("Metadata: dictionary {}", metadata_ref);
                    for (key, value) in dict.iter() {
                        let key = String::from_utf8_lossy(key);
                        println!("  {}: {}", key, object_to_pdf(value));
                    }
                }
                other => {
                    println!("Metadata: non-stream {}", metadata_ref);
                    println!("  type: {}", object_to_pdf(&other));
                }
            }
        }
        None => println!("Metadata: <missing>"),
    }

    Ok(())
}

fn run_show_outline(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;

    let items = match outline::outline_items(&mut pdf) {
        Ok(items) => items,
        Err(error) => {
            eprintln!("Warning: {error}");
            Vec::new()
        }
    };

    println!("Outline:");
    if items.is_empty() {
        println!("  <empty>");
        return Ok(());
    }

    for (index, item) in items.iter().enumerate() {
        println!("{}{}: {}", "  ".repeat(item.depth), index + 1, item.title);
    }
    Ok(())
}

fn run_show_fonts(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;

    let font_refs = match fonts::font_entries(&mut pdf) {
        Ok(font_refs) => font_refs,
        Err(error) => {
            eprintln!("Warning: {error}");
            Default::default()
        }
    };

    println!("Fonts:");
    if font_refs.is_empty() {
        println!("  <none>");
        return Ok(());
    }

    for (name, font_obj) in font_refs {
        let font_name = String::from_utf8_lossy(&name);
        if let Object::Dictionary(dict) = font_obj {
            println!("  /{}", font_name);
            if let Some(font_type) = dict.get("Type") {
                println!("    type: {}", object_to_pdf(font_type));
            }
            if let Some(subtype) = dict.get("Subtype") {
                println!("    subtype: {}", object_to_pdf(subtype));
            }
            if let Some(base_font) = dict.get("BaseFont") {
                println!("    base_font: {}", object_to_pdf(base_font));
            }
        } else {
            println!("  /{}", font_name);
            println!("    type: <invalid>");
            println!("    subtype: <invalid>");
            println!("    base_font: <invalid>");
        }
    }

    Ok(())
}

fn run_show_npages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let pages = pages::page_refs(&mut pdf)?;
    println!("{}", pages.len());
    Ok(())
}

fn run_show_pages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let page_refs = pages::page_refs(&mut pdf)?;
    for (index, page_ref) in page_refs.iter().enumerate() {
        let page = pdf.resolve(*page_ref)?;
        let Object::Dictionary(dict) = page else {
            continue;
        };

        println!("page {}: {}", index + 1, page_ref);
        if let Some(media_box) = dict.get("MediaBox") {
            println!("  media-box: {}", object_to_pdf(media_box));
        }
        if let Some(resources) = dict.get("Resources") {
            println!("  resources: {}", object_to_pdf(resources));
        }
        if let Some(contents) = dict.get("Contents") {
            println!("  contents: {}", object_to_pdf(contents));
        }
        if let Some(rotate) = dict.get("Rotate") {
            println!("  rotate: {}", object_to_pdf(rotate));
        }
    }

    Ok(())
}

fn open_pdf(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    let file = File::open(input)?;
    let pdf = Pdf::open_with_options(BufReader::new(file), pdf_open_options(repair, password)?)
        .map_err(actionable_password_error)?;

    for diagnostic in pdf.repair_diagnostics().entries() {
        eprintln!("warning: {}", diagnostic.message);
    }

    Ok(pdf)
}

fn pdf_open_options(repair: bool, password: &PasswordArgs) -> CliResult<PdfOpenOptions> {
    let password = if let Some(password) = &password.password {
        password.as_bytes().to_vec()
    } else if let Some(path) = &password.password_file {
        let mut bytes = std::fs::read(path)?;
        if bytes.ends_with(b"\r\n") {
            bytes.truncate(bytes.len() - 2);
        } else if bytes.ends_with(b"\n") {
            bytes.truncate(bytes.len() - 1);
        }
        bytes
    } else {
        Vec::new()
    };

    Ok(PdfOpenOptions { repair, password })
}

fn actionable_password_error(error: flpdf::Error) -> Box<dyn std::error::Error> {
    if matches!(
        error,
        flpdf::Error::Encrypted(flpdf::EncryptedError::BadPassword)
    ) {
        return "encrypted PDF: incorrect password; retry with --password or --password-file"
            .into();
    }
    error.into()
}

fn object_to_pdf(object: &Object) -> String {
    let mut out = Vec::new();
    object.write_pdf(&mut out);
    String::from_utf8_lossy(&out).to_string()
}
