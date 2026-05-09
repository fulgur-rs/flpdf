use clap::Parser;
use flpdf::{check_reader, write_pdf, Object, ObjectRef, Pdf, Severity};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;
type PageSequence = (Pdf<BufReader<File>>, Vec<ObjectRef>);

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
struct Args {
    #[arg(long)]
    check: bool,
    #[arg(long)]
    repair: bool,
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
    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    let result = if let Some(object_ref) = args.dump_object.as_deref() {
        run_dump_object(args.input, object_ref)
    } else if args.show_info {
        run_show_info(args.input)
    } else if args.show_catalog {
        run_show_catalog(args.input)
    } else if args.show_metadata {
        run_show_metadata(args.input)
    } else if args.show_outline {
        run_show_outline(args.input)
    } else if args.show_fonts {
        run_show_fonts(args.input)
    } else if args.show_npages {
        run_show_npages(args.input)
    } else if args.show_pages {
        run_show_pages(args.input)
    } else if args.check {
        run_check(args.input)
    } else {
        run_rewrite(args.input, args.output, args.repair)
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
    repair: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let file = File::open(input)?;
    let mut pdf = if repair {
        Pdf::open_with_repair(BufReader::new(file))?
    } else {
        Pdf::open(BufReader::new(file))?
    };
    for diagnostic in pdf.repair_diagnostics().entries() {
        eprintln!("warning: {}", diagnostic.message);
    }
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

fn run_show_info(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input)?;
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

fn run_show_catalog(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve(catalog_ref)?;
    println!("Catalog: {}", object_to_pdf(&catalog));
    Ok(())
}

fn run_show_metadata(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input)?;
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

fn run_show_outline(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog else {
        return Err(format!("document catalog {} is not a dictionary", catalog_ref).into());
    };

    let outlines = catalog
        .get_ref("Outlines")
        .ok_or("document has no outlines")?;
    let outline_root = pdf.resolve(outlines)?;
    let Object::Dictionary(outline_root) = outline_root else {
        return Err(format!("outlines object {} is not a dictionary", outlines).into());
    };

    println!("Outline:");
    let mut counter = 1usize;
    let mut visited = BTreeSet::new();
    const MAX_OUTLINE_DEPTH: usize = 100;
    if let Some(first) = outline_root.get_ref("First") {
        if let Err(error) = dump_outline_items(
            &mut pdf,
            first,
            0,
            &mut visited,
            &mut counter,
            MAX_OUTLINE_DEPTH,
        ) {
            eprintln!("Warning: {error}");
        }
    }
    if counter == 1 {
        println!("  <empty>");
    }
    Ok(())
}

fn run_show_fonts(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog else {
        return Err(format!("document catalog {} is not a dictionary", catalog_ref).into());
    };
    let pages_ref = catalog
        .get_ref("Pages")
        .ok_or("document pages tree missing")?;

    let mut seen_nodes = BTreeSet::new();
    let mut font_refs: BTreeMap<Vec<u8>, Object> = BTreeMap::new();
    const MAX_PAGE_TREE_DEPTH: usize = 100;
    if let Err(error) = collect_font_resources(
        &mut pdf,
        pages_ref,
        &mut seen_nodes,
        &mut font_refs,
        0,
        MAX_PAGE_TREE_DEPTH,
    ) {
        eprintln!("Warning: {error}");
    }

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

fn run_show_npages(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let (_pdf, pages) = load_page_sequence(input)?;
    println!("{}", pages.len());
    Ok(())
}

fn run_show_pages(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let (mut pdf, pages) = load_page_sequence(input)?;
    for (index, page_ref) in pages.iter().enumerate() {
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

fn dump_outline_items(
    pdf: &mut Pdf<BufReader<File>>,
    start: ObjectRef,
    depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
    counter: &mut usize,
    max_depth: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if depth >= max_depth {
        return Err(format!(
            "outline depth exceeds maximum of {} at {}",
            max_depth, start
        )
        .into());
    }

    let mut current = Some(start);
    while let Some(current_ref) = current {
        if depth >= max_depth {
            return Err(format!(
                "outline depth exceeds maximum of {} at {}",
                max_depth, current_ref
            )
            .into());
        }

        if !visited.insert(current_ref) {
            break;
        }

        let current_obj = pdf.resolve(current_ref)?;
        let Object::Dictionary(dict) = current_obj else {
            break;
        };

        let title = match dict.get("Title") {
            Some(Object::String(value)) => String::from_utf8_lossy(value).to_string(),
            Some(other) => object_to_pdf(other).to_string(),
            None => String::from("<untitled>"),
        };

        println!("{}{}: {}", "  ".repeat(depth), counter, title);
        *counter += 1;

        if let Some(first) = dict.get_ref("First") {
            dump_outline_items(pdf, first, depth + 1, visited, counter, max_depth)?;
        }

        current = dict.get_ref("Next");
    }

    Ok(())
}

fn collect_font_resources(
    pdf: &mut Pdf<BufReader<File>>,
    node: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
    fonts: &mut BTreeMap<Vec<u8>, Object>,
    depth: usize,
    max_depth: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if depth >= max_depth {
        return Err(format!(
            "page tree depth exceeds maximum of {} at {}",
            max_depth, node
        )
        .into());
    }

    if !seen.insert(node) {
        return Ok(());
    }

    let node_obj = pdf.resolve(node)?;
    let Object::Dictionary(dict) = node_obj else {
        return Ok(());
    };

    let node_type = dict
        .get("Type")
        .and_then(|value| match value {
            Object::Name(value) => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_else(Vec::new);

    if node_type.as_slice() == b"Pages" {
        if let Some(Object::Array(kids)) = dict.get("Kids") {
            for kid in kids {
                if let Object::Reference(reference) = kid {
                    collect_font_resources(pdf, *reference, seen, fonts, depth + 1, max_depth)?;
                }
            }
        }
        return Ok(());
    }

    if node_type.as_slice() == b"Page" {
        let resources = match dict.get("Resources") {
            Some(Object::Dictionary(resources)) => Some(resources.clone()),
            Some(Object::Reference(reference)) => {
                if let Ok(Object::Dictionary(resources)) = pdf.resolve(*reference) {
                    Some(resources)
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(resources) = resources {
            let fonts_dict = match resources.get("Font") {
                Some(Object::Dictionary(fonts_dict)) => Some(fonts_dict.clone()),
                Some(Object::Reference(reference)) => {
                    if let Ok(Object::Dictionary(fonts_dict)) = pdf.resolve(*reference) {
                        Some(fonts_dict)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(fonts_dict) = fonts_dict {
                for (font_name, value) in fonts_dict.iter() {
                    if let Object::Reference(font_ref) = value {
                        if let Ok(font_obj) = pdf.resolve(*font_ref) {
                            fonts.insert(font_name.to_vec(), font_obj);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn open_pdf(input: &PathBuf) -> CliResult<Pdf<BufReader<File>>> {
    let file = File::open(input)?;
    let pdf = Pdf::open(BufReader::new(file))?;
    Ok(pdf)
}

fn load_page_sequence(input: Option<PathBuf>) -> CliResult<PageSequence> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog else {
        return Err(format!("document catalog {} is not a dictionary", catalog_ref).into());
    };

    let pages_ref = catalog
        .get_ref("Pages")
        .ok_or("document pages tree missing")?;

    let mut seen = BTreeSet::new();
    let mut pages = Vec::new();
    const MAX_PAGE_TREE_DEPTH: usize = 100;
    collect_pages(
        &mut pdf,
        pages_ref,
        &mut seen,
        &mut pages,
        0,
        MAX_PAGE_TREE_DEPTH,
    )?;

    Ok((pdf, pages))
}

fn collect_pages(
    pdf: &mut Pdf<BufReader<File>>,
    node: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
    pages: &mut Vec<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if depth >= max_depth {
        return Err(format!(
            "page tree depth exceeds maximum of {} at {}",
            max_depth, node
        )
        .into());
    }

    if !seen.insert(node) {
        return Ok(());
    }

    let node_obj = pdf.resolve(node)?;
    let Object::Dictionary(dict) = node_obj else {
        return Ok(());
    };

    let node_type = dict
        .get("Type")
        .and_then(|value| match value {
            Object::Name(value) => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_else(Vec::new);

    if node_type.as_slice() == b"Pages" {
        if let Some(Object::Array(kids)) = dict.get("Kids") {
            for kid in kids {
                if let Object::Reference(reference) = kid {
                    collect_pages(pdf, *reference, seen, pages, depth + 1, max_depth)?;
                }
            }
        }
        return Ok(());
    }

    if node_type.as_slice() == b"Page" {
        pages.push(node);
    }

    Ok(())
}

fn object_to_pdf(object: &Object) -> String {
    let mut out = Vec::new();
    object.write_pdf(&mut out);
    String::from_utf8_lossy(&out).to_string()
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
