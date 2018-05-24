//! Defines the CPU target.
mod context;
mod cpu;
//mod mem_model;
mod printer;
mod compile;
mod cpu_argument;

pub use self::context::Context;
pub use self::cpu::Cpu;

use codegen;
use ir;
use num::bigint::BigInt;
use num::rational::Ratio;
use num::ToPrimitive;
use utils::*;

#[derive(Default)]
struct Namer {
    num_var: HashMap<ir::Type, usize>,
    num_sizes: usize,
    num_glob_ptr: usize,
}

impl Namer {
    /// Generate a variable name prefix from a type.
    fn gen_prefix(t: &ir::Type) -> &'static str {
        match *t {
            ir::Type::I(1) => "p",
            ir::Type::I(8) => "c",
            ir::Type::I(16) => "s",
            ir::Type::I(32) => "r",
            ir::Type::I(64) => "rd",
            ir::Type::F(16) => "h",
            ir::Type::F(32) => "f",
            ir::Type::F(64) => "d",
            ir::Type::PtrTo(..) =>  "ptr",
            _ => panic!("invalid CPU type"),
        }
    }
}

impl codegen::Namer for Namer {
    fn name(&mut self, t: ir::Type) -> String {
        let prefix = Namer::gen_prefix(&t);
        match t {
            ir::Type::PtrTo(..) => {
                let name = format!("{}{}", prefix, self.num_glob_ptr);
                self.num_glob_ptr += 1;
                name
            }
            _ => {
                let entry = self.num_var.entry(t).or_insert(0);
                let name = format!("{}{}", prefix, *entry);
                *entry += 1;
                name
            }
        }
    }

    fn name_float(&self, val: &Ratio<BigInt>, len: u16) -> String {
        assert!(len <= 64);
        let f = unwrap!(val.numer().to_f64()) / unwrap!(val.denom().to_f64());
        //let binary = unsafe { std::mem::transmute::<f64, u64>(f) };
        format!("{:.5e}", f )
    }

    fn name_int(&self, val: &BigInt, len: u16) -> String {
        assert!(len <= 64);
        format!("{}", unwrap!(val.to_i64()))
    }

    fn name_param(&mut self, p: codegen::ParamValKey) -> String {
        match p {
            codegen::ParamValKey::External(p) => p.name.clone(),
            codegen::ParamValKey::GlobalMem(mem) => format!("_gbl_mem_{}", mem.0),
            codegen::ParamValKey::Size(_) => {
                self.num_sizes += 1;
                format!("_size_{}", self.num_sizes-1)
            },
        }
    }
}
