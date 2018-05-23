#[cfg(test)] extern crate env_logger;
extern crate handlebars;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate itertools;
extern crate regex;
extern crate rustfmt;
extern crate serde;
#[macro_use] extern crate serde_derive;
extern crate serde_json;
#[macro_use]
extern crate telamon_utils as utils;
extern crate topological_sort;
extern crate libc;

extern crate lalrpop_util;

mod ast;
mod constraint;
mod flat_filter;
pub mod ir;
pub mod lexer;
generated_file!(pub parser);
mod print;
mod truth_table;
pub mod error;

use std::{fs, io, path};

use utils::*;

/// Converts a choice name to a rust type name.
fn to_type_name(name: &str) -> String {
    let mut result = "".to_string();
    let mut is_new_word = true;
    let mut last_is_sep = true;
    for c in name.chars() {
        if c != '_' || last_is_sep {
            if is_new_word {
                result.extend(c.to_uppercase());
            } else {
                result.push(c);
            }
        }
        last_is_sep = c == '_';
        is_new_word = last_is_sep || c.is_numeric();
    }
    result
}

/// Process a file and stores the result in an other file.
pub fn process_file<'a>(
    input_path: &'a path::Path,
    output_path: &path::Path,
    format: bool
) -> Result<(), error::ProcessError<'a>> {
    let mut input = fs::File::open(path::Path::new(input_path)).unwrap();
    let mut output = fs::File::create(path::Path::new(output_path)).unwrap();
    let input_path_str = input_path.to_string_lossy();
    info!("compiling {} to {}", input_path_str, output_path.to_string_lossy());
    process(&mut input, &mut output, format, input_path)
}

/// Parses a constraint description file.
pub fn process<'a, T: io::Write>(
    input: &mut io::Read,
    output: &mut T,
    format: bool,
    input_path: &'a path::Path
) -> Result<(), error::ProcessError<'a>> {
    // Parse and check the input.
    let tokens = lexer::Lexer::new(input);
    let ast: ast::Ast =
        parser::parse_ast(tokens)
               .map_err(|c| error::ProcessError::from((input_path.display(), c)))?;
    let (mut ir_desc, constraints) = ast.type_check();
    debug!("constraints: {:?}", constraints);
    // Generate flat filters.
    let mut filters = MultiHashMap::default();
    for mut constraint in constraints {
        constraint.dedup_inputs(&ir_desc);
        for (choice, filter) in constraint.gen_filters(&ir_desc) {
            filters.insert(choice, filter);
        }
    }
    debug!("filters: {:?}", filters);
    // Merge and generate structured filters.
    for (choice, filters) in filters {
        for filter in flat_filter::merge(filters, &ir_desc) {
            debug!("compiling filter for choice {}: {:?}", choice, filter);
            let (vars, inputs, rules, set_constraints) = filter.deconstruct();
            let rules = truth_table::opt_rules(&inputs, rules, &ir_desc);
            let arguments = ir_desc.get_choice(&choice).arguments().sets().enumerate()
                .map(|(id, set)| (ir::Variable::Arg(id), set))
                .map(|(v, set)| set_constraints.find_set(v).unwrap_or(set))
                .chain(&vars).cloned().collect();
            let new_filter = ir::Filter { arguments, rules, inputs };
            debug!("adding filter to {}: {:?}", choice, new_filter);
            ir_desc.add_filter(choice.clone(), new_filter, vars, set_constraints);
        }
    }
    // Print and format the output.
    let code = print::print(&ir_desc);
    if format {
        let fmt_input = rustfmt::Input::Text(code);
        let mut fmt_config = rustfmt::config::Config::default();
        fmt_config.set().write_mode(rustfmt::config::WriteMode::Plain);
        fmt_config.set().wrap_comments(true);
        fmt_config.set().take_source_hints(false);
        fmt_config.set().single_line_if_else_max_width(90);
        fmt_config.set().reorder_imported_names(true);
        fmt_config.set().reorder_imports(true);
        fmt_config.set().fn_single_line(true);
        fmt_config.set().struct_variant_width(90);
        fmt_config.set().struct_lit_width(90);
        fmt_config.set().fn_call_width(90);
        fmt_config.set().max_width(90);
        let fmt_res = rustfmt::format_input(fmt_input, &fmt_config, Some(output));
        let (_, _, fmt_report) = fmt_res.unwrap();
        if fmt_report.has_warnings() { println!("{}", fmt_report); }
    } else {
        write!(output, "{}", code).unwrap();
    }
    Ok(())
}

// TODO(cleanup): avoid name conflicts in the printer
// TODO(feature): allow multiple supersets
// TODO(filter): group filters if one iterates on a subtype of the other
// TODO(filter): generate negative filter when there is at most one input.
// TODO(filter): merge filters even if one input requires a type constraint
// TODO(cc_perf): remove duplicates on_change filter actions
// - normalize filter calls
//   -> can inverse lhs and rhs in symmetric, be careful if self is symmetric
// - detect duplicates and remove them
// - IS A X2 LIMITING FACTOR
// > couldn't we do this during merging ?
// TODO(cc_perf): Only iterate on the lower triangular part of symmetric domains in
// on_change
// TODO(cc_perf): in truth table, intersect rules with rules with weaker conditions,

// FIXME: fix counters:
// * Discard full counters. Might re-enable them later if we can find a way to make lowerings
//  commute
// * Fordid resrtricting the FALSE value of the repr flag from conditions that involve other decisions
// > this makes lowering commute. Otherwise we can force the counter to be >0, which can be true or
//   not depending if another lowering has already occured.
// * Charaterise the effect of fragile values on performance
// FIXME: make sure the quotient set works correctly with things that force the repr flag to TRUE
// (for example the constraint on reduction). One problem might be that the flag is forced to FALSE
// but can be merged with a dim whose flag can be set to TRUE. The solutions for this may be to
// ensure:
// - conditions on flags are uniform for merged dims
// - the flag has a third state "ok to be true, but onyl if merged to another"
