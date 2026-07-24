use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    run(&args)
}

fn run(args: &[String]) -> ExitCode {
    let whoami = program_name(
        args.first()
            .map(String::as_str)
            .unwrap_or("qpdf-test-compare"),
    );
    if args.len() == 2 && args[1] == "--version" {
        println!("{whoami} from flpdf version {}", flpdf::version());
        return ExitCode::from(0);
    }
    // Real logic lands in later tasks. Exit 2 (qpdf's default error code for
    // compare-for-test) so any accidental invocation of the stub fails loud.
    ExitCode::from(2)
}

fn program_name(argv0: &str) -> &str {
    argv0.rsplit('/').next().unwrap_or(argv0)
}
