/// Tool that generates constraints from stdin to stdout.
extern crate telamon_gen;
extern crate lalrpop_util;
extern crate env_logger;

use lalrpop_util::ParseError;

use std::process;
use std::path::Path;

fn main() {
    env_logger::init();
    if let Err((ParseError::User { error }, filename)) = telamon_gen::process(
        &mut std::io::stdin(),
        &mut std::io::stdout(),
        true,
        &Path::new("std")
    ) {
        eprintln!("{}: {}", filename, error);
        process::exit(-1);
    }
}
