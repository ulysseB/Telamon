//! Defines operators.
use self::Operator::*;
use crate::ir::{self, AccessPattern, LoweringMap, Operand, Type};
use fxhash::FxHashSet;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::{self, fmt};

/// The rounding mode of an arithmetic operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(C)]
pub enum Rounding {
    /// No rounding occurs.
    Exact,
    /// Rounds toward the nearest number.
    Nearest,
    /// Rounds toward zero.
    Zero,
    /// Rounds toward positive infinite.
    Positive,
    /// Rounds toward negative infinite.
    Negative,
}

impl std::fmt::Display for Rounding {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let name = match self {
            Rounding::Exact => "exact",
            Rounding::Nearest => "toward nearest",
            Rounding::Zero => "toward zero",
            Rounding::Positive => "toward +inf",
            Rounding::Negative => "toward -inf",
        };
        write!(f, "{}", name)
    }
}

impl Rounding {
    /// Ensures the rounding policy applies to the given type.
    fn check(self, t: ir::Type) -> Result<(), ir::TypeError> {
        if t.is_float() ^ (self == Rounding::Exact) {
            Ok(())
        } else {
            Err(ir::TypeError::InvalidRounding { rounding: self, t })
        }
    }
}

/// Represents binary arithmetic operators.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[repr(C)]
pub enum BinOp {
    /// Adds two operands.
    Add,
    /// Substracts two operands.
    Sub,
    /// Divides two operands,
    Div,
    /// Computes the bitwise AND operation.
    And,
    /// Computes the bitwise OR operation.
    Or,
    /// Computes `lhs < rhs`.
    Lt,
    /// Computes `lhs <= rhs`.
    Leq,
    /// Computes `lhs == rhs`.
    Equals,
    /// Computes max(lhs, rhs)
    Max,
}

impl fmt::Display for BinOp {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.write_str(self.name())
    }
}

impl BinOp {
    /// Returns a string representing the operator.
    fn name(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Div => "div",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Lt => "lt",
            BinOp::Leq => "leq",
            BinOp::Equals => "equals",
            BinOp::Max => "max",
        }
    }

    /// Returns the type of the binay operator given the type of its operands.
    pub fn t(self, operand_type: ir::Type) -> ir::Type {
        match self {
            BinOp::Lt | BinOp::Leq | BinOp::Equals => ir::Type::I(1),
            _ => operand_type,
        }
    }

    /// Indicates if the result must be rounded when operating on floats.
    fn requires_rounding(self) -> bool {
        match self {
            BinOp::Lt | BinOp::Leq | BinOp::Equals | BinOp::Max => false,
            _ => true,
        }
    }
}

/// Arithmetic operators with a single operand.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[repr(C)]
pub enum UnaryOp {
    /// Simply copy the input.
    Mov,
    /// Casts the input to another type.
    Cast(ir::Type),
    /// Calculates exp(x)
    Exp(ir::Type),
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UnaryOp::Exp(..) => fmt.write_str("exp"),
            UnaryOp::Mov => fmt.write_str("mov"),
            UnaryOp::Cast(t) => write!(fmt, "cast({})", t),
        }
    }
}

impl UnaryOp {
    /// Gives the return type of the operand given its input type.
    fn t(self, op_type: ir::Type) -> ir::Type {
        match self {
            UnaryOp::Mov | UnaryOp::Exp(..) => op_type,
            UnaryOp::Cast(t) => t,
        }
    }
}

/// The operation performed by an instruction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Operator<L = LoweringMap> {
    /// A binary arithmetic operator.
    BinOp(BinOp, Operand<L>, Operand<L>, Rounding),
    /// Unary arithmetic operator.
    UnaryOp(UnaryOp, Operand<L>),
    /// Performs a multiplication with the given return type.
    Mul(Operand<L>, Operand<L>, Rounding, Type),
    /// Performs s multiplication between the first two operands and adds the
    /// result to the third.
    Mad(Operand<L>, Operand<L>, Operand<L>, Rounding),
    /// Loads a value of the given type from the given address.
    Ld(Type, Operand<L>, AccessPattern),
    /// Stores the second operand at the address given by the first.
    /// The boolean specifies if the instruction has side effects. A store has no side
    /// effects when it writes into a cell that previously had an undefined value.
    St(Operand<L>, Operand<L>, bool, AccessPattern),
    /// Represents a load from a temporary memory that is not fully defined yet.
    TmpLd(Type, ir::MemId),
    /// Represents a store to a temporary memory that is not fully defined yet.
    TmpSt(Operand<L>, ir::MemId),
}

impl<L> Operator<L> {
    /// Ensures the types of the operands are valid.
    pub fn check(
        &self,
        iter_dims: &FxHashSet<ir::DimId>,
        fun: &ir::Function<L>,
    ) -> Result<(), ir::Error> {
        self.t()
            .map(|t| fun.device().check_type(t))
            .unwrap_or(Ok(()))?;
        for operand in self.operands() {
            fun.device().check_type(operand.t())?;
            // Ensure dimension mappings are registered.
            if let Some(dim_map) = operand.mapped_dims() {
                for &(lhs, rhs) in dim_map {
                    if fun.find_mapping(lhs, rhs).is_none() {
                        Err(ir::Error::MissingDimMapping { lhs, rhs })?;
                    }
                }
            }
        }
        match *self {
            BinOp(operator, ref lhs, ref rhs, rounding) => {
                if operator.requires_rounding() {
                    rounding.check(lhs.t())?;
                } else if rounding != Rounding::Exact {
                    Err(ir::TypeError::InvalidRounding {
                        rounding,
                        t: lhs.t(),
                    })?;
                }
                ir::TypeError::check_equals(lhs.t(), rhs.t())?;
            }
            Mul(ref lhs, ref rhs, rounding, res_type) => {
                rounding.check(lhs.t())?;
                ir::TypeError::check_equals(lhs.t(), rhs.t())?;
                match (lhs.t(), res_type) {
                    (x, z) if x == z => (),
                    (Type::I(32), Type::I(64)) | (Type::I(32), Type::PtrTo(_)) => (),
                    (_, t) => Err(ir::TypeError::UnexpectedType { t })?,
                }
            }
            Mad(ref mul_lhs, ref mul_rhs, ref add_rhs, rounding) => {
                rounding.check(mul_lhs.t())?;
                ir::TypeError::check_equals(mul_lhs.t(), mul_rhs.t())?;
                match (mul_lhs.t(), add_rhs.t()) {
                    (ref x, ref z) if x == z => (),
                    (Type::I(32), Type::I(64)) | (Type::I(32), Type::PtrTo(_)) => (),
                    (_, t) => Err(ir::TypeError::UnexpectedType { t })?,
                }
            }
            Ld(_, ref addr, ref pattern) => {
                pattern.check(iter_dims)?;
                let pointer_type = pattern.pointer_type(fun.device());
                ir::TypeError::check_equals(addr.t(), pointer_type)?;
            }
            St(ref addr, _, _, ref pattern) => {
                pattern.check(iter_dims)?;
                let pointer_type = pattern.pointer_type(fun.device());
                ir::TypeError::check_equals(addr.t(), pointer_type)?;
            }
            TmpLd(..) | UnaryOp(..) | TmpSt(..) => (),
        }
        Ok(())
    }

    /// Returns the type of the value produced.
    pub fn t(&self) -> Option<Type> {
        match self {
            Mad(_, _, op, _) => Some(op.t()),
            Ld(t, ..) | TmpLd(t, _) | Mul(.., t) => Some(*t),
            BinOp(operator, lhs, ..) => Some(operator.t(lhs.t())),
            UnaryOp(operator, operand) => Some(operator.t(operand.t())),
            St(..) | TmpSt(..) => None,
        }
    }

    /// Retruns the list of operands.
    pub fn operands(&self) -> Vec<&Operand<L>> {
        match self {
            BinOp(_, lhs, rhs, _) | Mul(lhs, rhs, _, _) | St(lhs, rhs, _, _) => {
                vec![lhs, rhs]
            }
            Mad(mul_lhs, mul_rhs, add_rhs, _) => vec![mul_lhs, mul_rhs, add_rhs],
            UnaryOp(_, op) | Ld(_, op, _) | TmpSt(op, _) => vec![op],
            TmpLd(..) => vec![],
        }
    }

    /// Retruns the list of mutable references to operands.
    pub fn operands_mut<'b>(&'b mut self) -> Vec<&'b mut Operand<L>> {
        match self {
            BinOp(_, lhs, rhs, _) | Mul(lhs, rhs, _, _) | St(lhs, rhs, _, _) => {
                vec![lhs, rhs]
            }
            Mad(mul_lhs, mul_rhs, add_rhs, _) => vec![mul_lhs, mul_rhs, add_rhs],
            UnaryOp(_, op, ..) | Ld(_, op, ..) | TmpSt(op, _) => vec![op],
            TmpLd(..) => vec![],
        }
    }

    /// Returns true if the operator has side effects.
    pub fn has_side_effects(&self) -> bool {
        match self {
            St(_, _, b, _) => *b,
            BinOp(..) | UnaryOp(..) | Mul(..) | Mad(..) | Ld(..) | TmpLd(..)
            | TmpSt(..) => false,
        }
    }

    /// Indicates if the operator accesses memory.
    pub fn is_mem_access(&self) -> bool {
        match self {
            St(..) | Ld(..) | TmpSt(..) | TmpLd(..) => true,
            _ => false,
        }
    }

    /// Renames a basic block.
    pub fn merge_dims(&mut self, lhs: ir::DimId, rhs: ir::DimId) {
        self.operands_mut()
            .iter_mut()
            .for_each(|x| x.merge_dims(lhs, rhs));
    }

    /// Returns the pattern of access to the memory by the instruction, if any.
    pub fn mem_access_pattern(&self) -> Option<Cow<AccessPattern>> {
        match *self {
            Ld(_, _, ref pattern) | St(_, _, _, ref pattern) => {
                Some(Cow::Borrowed(pattern))
            }
            TmpLd(_, mem_id) | TmpSt(_, mem_id) => {
                Some(Cow::Owned(AccessPattern::Unknown(Some(mem_id))))
            }
            _ => None,
        }
    }

    /// Returns the memory blocks referenced by the instruction.
    pub fn mem_used(&self) -> Option<ir::MemId> {
        self.mem_access_pattern().and_then(|p| p.mem_block())
    }

    pub fn map_operands<T, F>(self, mut f: F) -> Operator<T>
    where
        F: FnMut(Operand<L>) -> Operand<T>,
    {
        match self {
            BinOp(op, oper1, oper2, rounding) => {
                let oper1 = f(oper1);
                let oper2 = f(oper2);
                BinOp(op, oper1, oper2, rounding)
            }
            UnaryOp(operator, operand) => UnaryOp(operator, f(operand)),
            Mul(oper1, oper2, rounding, t) => {
                let oper1 = f(oper1);
                let oper2 = f(oper2);
                Mul(oper1, oper2, rounding, t)
            }
            Mad(oper1, oper2, oper3, rounding) => {
                let oper1 = f(oper1);
                let oper2 = f(oper2);
                let oper3 = f(oper3);
                Mad(oper1, oper2, oper3, rounding)
            }
            Ld(t, oper1, ap) => {
                let oper1 = f(oper1);
                Ld(t, oper1, ap)
            }
            St(oper1, oper2, side_effects, ap) => {
                let oper1 = f(oper1);
                let oper2 = f(oper2);
                St(oper1, oper2, side_effects, ap)
            }
            TmpLd(t, id) => TmpLd(t, id),
            TmpSt(oper1, id) => {
                let oper1 = f(oper1);
                TmpSt(oper1, id)
            }
        }
    }
}

impl<L> ir::IrDisplay<L> for Operator<L> {
    fn fmt(&self, fmt: &mut fmt::Formatter, function: &ir::Function<L>) -> fmt::Result {
        match self {
            BinOp(op, lhs, rhs, _rnd) => write!(
                fmt,
                "{}({}, {})",
                op,
                lhs.display(function),
                rhs.display(function)
            ),
            UnaryOp(op, arg) => write!(fmt, "{}({})", op, arg.display(function)),
            Mul(lhs, rhs, _rnd, _t) => write!(
                fmt,
                "mul({}, {})",
                lhs.display(function),
                rhs.display(function)
            ),
            Mad(arg0, arg1, arg2, _rnd) => write!(
                fmt,
                "mad({}, {}, {})",
                arg0.display(function),
                arg1.display(function),
                arg2.display(function)
            ),
            Ld(_t, arg, _ap) => write!(fmt, "load({})", arg.display(function)),
            St(dst, src, _side_effects, _ap) => write!(
                fmt,
                "store({}, {})",
                dst.display(function),
                src.display(function)
            ),
            TmpLd(_t, mem) => write!(fmt, "load({})", mem),
            TmpSt(src, mem) => write!(fmt, "store({}, {})", mem, src.display(function)),
        }
    }
}

impl Operator<()> {
    pub fn freeze(self, cnt: &mut ir::Counter) -> Operator {
        self.map_operands(|oper| oper.freeze(cnt))
    }
}

impl<L> std::fmt::Display for Operator<L> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            BinOp(op, lhs, rhs, rnd) => write!(fmt, "{}[{}]({}, {})", op, rnd, lhs, rhs),
            UnaryOp(op, arg) => write!(fmt, "{}({})", op, arg),
            Mul(lhs, rhs, rnd, t) => write!(fmt, "Mul<{}>[{}]({}, {})", t, rnd, lhs, rhs),
            Mad(arg0, arg1, arg2, rnd) => {
                write!(fmt, "Mad[{}]({}, {}, {})", rnd, arg0, arg1, arg2)
            }
            Ld(_t, arg, _ap) => write!(fmt, "Load({})", arg),
            St(dst, src, _side_effects, _ap) => write!(fmt, "Store({}, {})", dst, src),
            TmpLd(_t, mem) => write!(fmt, "TempLoad({})", mem),
            TmpSt(src, mem) => write!(fmt, "TempStore({}, {})", mem, src),
        }
    }
}
