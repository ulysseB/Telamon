use std::fmt;
use std::sync::Arc;

use num;
use utils::unwrap;

use crate::ir;
use crate::search_space::{NumSet, SearchSpace};

/// The size of an iteration dimension. The size is of the form:
/// `(factor * dividend_0 * dividend_1 * ...)) / divisor`
/// where the reminder of the division is null.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Size {
    factor: u32,
    dividend: Vec<Arc<ir::Parameter>>,
    divisor: u32,
}

impl Size {
    /// Creates a new 'Size'.
    pub fn new(factor: u32, dividend: Vec<Arc<ir::Parameter>>, divisor: u32) -> Self {
        assert!(divisor != 0);
        let mut new = Size {
            factor,
            dividend,
            divisor,
        };
        new.simplify();
        new
    }

    /// Converts an `ir::Size` to `Self`.
    pub fn from_ir(size: &ir::PartialSize, space: &SearchSpace) -> Self {
        let (cst_factor, param_factors, dim_size_factors) = size.factors();
        let dim_size_divisors = size.divisors();
        let factor = cst_factor
            * dim_size_factors
                .iter()
                .map(|&d| dim_size(d, space))
                .product::<u32>();
        let divisor = dim_size_divisors
            .iter()
            .map(|&d| dim_size(d, space))
            .product();
        Size::new(
            factor,
            param_factors.into_iter().cloned().collect(),
            divisor,
        )
    }

    /// Returns the size of a dimension if it is staticaly known.
    pub fn as_int(&self) -> Option<u32> {
        if self.dividend.is_empty() {
            Some(self.factor)
        } else {
            None
        }
    }

    /// Returns the dividends.
    pub fn dividend(&self) -> &[Arc<ir::Parameter>] {
        &self.dividend
    }

    /// Returns the divisor.
    pub fn divisor(&self) -> u32 {
        self.divisor
    }

    /// Returns the factor.
    pub fn factor(&self) -> u32 {
        self.factor
    }

    /// Simplifies the fraction factor/divisor.
    fn simplify(&mut self) {
        let gcd = num::integer::gcd(self.factor, self.divisor);
        self.factor /= gcd;
        self.divisor /= gcd;
    }
}

impl std::ops::MulAssign<&'_ Size> for Size {
    fn mul_assign(&mut self, rhs: &'_ Size) {
        self.factor *= rhs.factor;
        self.dividend.extend(rhs.dividend.iter().cloned());
        self.divisor *= rhs.divisor;
        self.simplify();
    }
}

impl fmt::Display for Size {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut pre = false;
        if self.factor != 1 {
            write!(fmt, "{}", self.factor)?;
            pre = true;
        }

        for p in &self.dividend {
            if pre {
                write!(fmt, "*")?;
            }
            write!(fmt, "{}", p)?;
            pre = true;
        }

        if self.divisor != 1 {
            write!(fmt, "/{}", self.divisor)?;
        }

        Ok(())
    }
}

/// Returns the size of a static dimension from the domain.
fn dim_size(dim: ir::DimId, space: &SearchSpace) -> u32 {
    let universe = unwrap!(space.ir_instance().dim(dim).possible_sizes());
    let size = space.domain().get_size(dim).as_constrained(universe);
    unwrap!(size, "dim {} is not constrained", dim)
}
