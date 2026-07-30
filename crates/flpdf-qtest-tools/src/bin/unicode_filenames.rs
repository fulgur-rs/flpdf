use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const BUF_SIZE: usize = 10240;

fn do_copy(input: &Path, output: &Path) {
    let f_in_res = File::open(input);
    let f_out_res = File::create(output);

    let mut f_in = f_in_res.unwrap_or_else(|_| {
        eprintln!("errors opening files");
        std::process::exit(2);
    });
    let mut f_out = f_out_res.unwrap_or_else(|_| {
        eprintln!("errors opening files");
        std::process::exit(2);
    });

    let mut buf = [0u8; BUF_SIZE];
    loop {
        let n = match f_in.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if f_out.write_all(&buf[..n]).is_err() {
            eprintln!("errors reading or writing");
            std::process::exit(2);
        }
    }
}

fn main() {
    let src = Path::new("minimal.pdf");
    let dst1 = Path::new("auto-\u{00fc}.pdf");
    let dst2 = Path::new("auto-\u{00f6}\u{03c0}.pdf");

    do_copy(src, dst1);
    do_copy(src, dst2);

    println!("created Unicode filenames");
}
