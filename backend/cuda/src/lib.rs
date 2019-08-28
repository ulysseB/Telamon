//! Defines the CUDA target.
#![deny(bare_trait_objects, unused_lifetimes)]
#![warn(clippy::all)]

#[cfg(feature = "real_gpu")]
mod api;
#[cfg(not(feature = "real_gpu"))]
mod api {
    mod error;
    mod fake;
    pub use self::error::*;
    pub use self::fake::*;
}
mod context;
mod gpu;
mod kernel;
mod mem_model;
mod printer;

#[cfg(feature = "real_gpu")]
pub mod characterize;

// Constructs to retrieve information on the GPU, that are not needed for the regular
// operation of Telamon and thus only present if the cuda feature is.
pub use self::api::{Array, Executor, JITDaemon};
#[cfg(feature = "real_gpu")]
pub use self::api::{DeviceAttribute, PerfCounter, PerfCounterSet};
pub use self::context::Context;
pub use self::gpu::{Gpu, InstDesc};
pub use self::kernel::Kernel;

use fxhash::FxHashMap;
use num::bigint::BigInt;
use num::rational::Ratio;
use num::ToPrimitive;
use telamon::codegen;
use telamon::ir;
use utils::*;

#[derive(Default)]
pub(crate) struct ValuePrinter {
    num_var: FxHashMap<ir::Type, usize>,
    num_sizes: usize,
}

impl ValuePrinter {
    /// Generate a variable name prefix from a type.
    fn gen_prefix(t: ir::Type) -> &'static str {
        match t {
            ir::Type::I(1) => "p",
            ir::Type::I(8) => "c",
            ir::Type::I(16) => "s",
            ir::Type::I(32) => "r",
            ir::Type::I(64) => "rd",
            ir::Type::F(16) => "h",
            ir::Type::F(32) => "f",
            ir::Type::F(64) => "d",
            _ => panic!("invalid PTX type"),
        }
    }
}

impl codegen::ValuePrinter for ValuePrinter {
    fn get_const_float(&self, val: &Ratio<BigInt>, len: u16) -> String {
        assert!(len <= 64);
        let f = unwrap!(val.numer().to_f64()) / unwrap!(val.denom().to_f64());
        let binary = unsafe { std::mem::transmute::<f64, u64>(f) };
        format!("0D{:016X}", binary)
    }

    fn get_const_int(&self, val: &BigInt, len: u16) -> String {
        assert!(len <= 64);
        format!("{}", unwrap!(val.to_i64()))
    }

    fn name(&mut self, t: ir::Type) -> String {
        let prefix = ValuePrinter::gen_prefix(t);
        let entry = self.num_var.entry(t).or_insert(0);
        let name = format!("%{}{}", prefix, *entry);
        *entry += 1;
        name
    }

    fn name_param(&mut self, p: codegen::ParamValKey) -> String {
        match p {
            codegen::ParamValKey::External(p) => p.name.clone(),
            codegen::ParamValKey::GlobalMem(mem) => format!("_gbl_mem_{}", mem.0),
            codegen::ParamValKey::Size(_) => {
                self.num_sizes += 1;
                format!("_size_{}", self.num_sizes - 1)
            }
        }
    }
}
