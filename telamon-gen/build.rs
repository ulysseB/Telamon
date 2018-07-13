extern crate cc;
extern crate lalrpop;


/// Adds a dependency to the build script.
fn add_dependency(dep: &str) { println!("cargo:rerun-if-changed={}", dep); }

fn main() {
    // Regenerate the lexer.(`LEX="flex" cargo build --features "lex"`)
    #[cfg(feature = "lex")]
    {
        use std::{env,process::Command};

        // Generate the lexer .             
        add_dependency("src/poc.l");
        let bin = env::var("LEX").unwrap_or(String::from("flex"));

        Command::new(bin)
                .arg("-osrc/exh.c")
                .arg("src/exh.l")
                .status()
                .expect("failed to execute Flex's process");
    }

    // Compile the lexer .             
    cc::Build::new()
            .file("src/exh.c")
            .include("src")
            .flag("-Wno-unused-parameter")
            .flag("-Wno-unused-variable")
            .flag_if_supported("-Wno-unused-function")
            .compile("exh.a");

    // Compile the parser.
    add_dependency("src/exh.c");
    add_dependency("src/parser.lalrpop");
    lalrpop::Configuration::new().use_cargo_dir_conventions().process().unwrap();
}
