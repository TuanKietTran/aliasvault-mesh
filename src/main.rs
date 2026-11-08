//! avmesh binary entry point: a thin shell over `avmesh::cli` so the
//! whole command surface is testable from the library.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    let code = avmesh::cli::run(&args, &mut out);
    let _ = out.flush();
    std::process::exit(code);
}
