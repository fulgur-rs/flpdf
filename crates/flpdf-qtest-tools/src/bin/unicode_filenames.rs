use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const BUF_SIZE: usize = 10240;

fn do_copy(input: &Path, output: &Path) {
    let mut f_in = match File::open(input) {
        Ok(f) => f,
        Err(_) => {
            eprint!("errors opening files\n");
            std::process::exit(2);
        }
    };

    let mut f_out = match File::create(output) {
        Ok(f) => f,
        Err(_) => {
            eprint!("errors opening files\n");
            std::process::exit(2);
        }
    };

    let mut buf = [0u8; BUF_SIZE];
    loop {
        match f_in.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if f_out.write_all(&buf[..n]).is_err() {
                    eprint!("errors reading or writing\n");
                    std::process::exit(2);
                }
            }
            Err(_) => {
                eprint!("errors reading or writing\n");
                std::process::exit(2);
            }
        }
    }
}

fn main() {
    let src = Path::new("minimal.pdf");
    let dst1 = Path::new("auto-\u{00fc}.pdf");
    let dst2 = Path::new("auto-\u{00f6}\u{03c0}.pdf");

    do_copy(src, dst1);
    do_copy(src, dst2);

    print!("created Unicode filenames\n");
}
