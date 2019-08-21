use crate::codegen;
use crate::ir;
use crate::search_space::{DimKind, Domain, Order, SearchSpace};
use itertools::Itertools;
use std;
use utils::*;

/// An iteration dimension composed of one or mure fused dimensions.
///
/// Note that induction levels are only associated with IR dimensions that are actually used by an
/// instruction.  For instance, if dimensions `%0` and `%2` are merged in the following code:
///
///   @0[%0]: load(a[%0])
///   @1[%1, %2]: load(b[%1])
///
/// then only `%0` would have an associated level in the corresponding iteration dimension.
///
/// Note that the representant does not necessarily have an associated induction level; in the
/// example above, the representant could be either `%0` or `%2` (with the other dimension in
/// `other_dims`).
#[derive(Debug)]
pub struct Dimension<'a> {
    /// The iteration kind.  This should be fully constrained.
    kind: DimKind,
    /// The representant.  This can be any of the fused IR dimensions represented by this
    /// dimension.
    representant: ir::DimId,
    /// The IR dimensions represented by this dimensions other than the representant.
    other_dims: Vec<ir::DimId>,
    /// The induction levels corresponding to this dimension.
    induction_levels: Vec<InductionLevel<'a>>,
    /// The size of this iteration dimension.
    size: codegen::Size<'a>,
}

impl<'a> Dimension<'a> {
    /// Returns the ID of the representant.
    pub fn id(&self) -> ir::DimId {
        self.representant
    }

    /// Returns the kind of the dimension.
    pub fn kind(&self) -> DimKind {
        self.kind
    }

    /// Returns the size of the dimensions.
    pub fn size(&self) -> &codegen::Size<'a> {
        &self.size
    }

    /// Returns the ids of the `ir::Dimensions` represented by this dimension.
    pub fn dim_ids(&self) -> impl Iterator<Item = ir::DimId> {
        std::iter::once(self.representant).chain(self.other_dims.clone())
    }

    /// Returns the induction levels handled by this loop.
    pub fn induction_levels(&self) -> &[InductionLevel<'a>] {
        &self.induction_levels
    }

    /// Gives the ownership on the induction levels computed by the dimension.
    pub fn drain_induction_levels(&mut self) -> Vec<InductionLevel<'a>> {
        std::mem::replace(&mut self.induction_levels, Vec::new())
    }

    /// Merge another `Dimension` into this one.
    pub fn merge_from(&mut self, other: Self) {
        assert_eq!(self.kind, other.kind);
        assert_eq!(self.size, other.size);
        self.other_dims.push(other.representant);
        self.other_dims.extend(other.other_dims);
        self.induction_levels.extend(other.induction_levels);
    }

    /// Returns the values to pass from the host to the device to implement `self`.
    pub fn host_values<'b>(
        &'b self,
        space: &'b SearchSpace,
    ) -> impl Iterator<Item = codegen::ParamVal<'a>> + 'b {
        let size_param = if self.kind == DimKind::LOOP {
            codegen::ParamVal::from_size(&self.size)
        } else {
            None
        };
        self.induction_levels
            .iter()
            .flat_map(move |l| l.host_values(space))
            .chain(size_param)
    }

    /// Creates a new dimension from an `ir::Dimension`.
    fn new(dim: &'a ir::Dimension, space: &SearchSpace) -> Self {
        let kind = space.domain().get_dim_kind(dim.id());
        assert!(kind.is_constrained());
        Dimension {
            kind,
            representant: dim.id(),
            size: codegen::Size::from_ir(dim.size(), space),
            other_dims: vec![],
            induction_levels: vec![],
        }
    }

    /// Adds `dim` to the list of fused dimensions if it is indeed the case.
    fn try_add_fused_dim(&mut self, dim: &ir::Dimension, space: &SearchSpace) -> bool {
        let order = space
            .domain()
            .get_order(self.representant.into(), dim.id().into());
        assert!(order.is_constrained());
        if order == Order::MERGED {
            self.other_dims.push(dim.id());
            debug_assert_eq!(self.kind, space.domain().get_dim_kind(dim.id()));
            debug_assert_eq!(self.size, codegen::Size::from_ir(dim.size(), space));
            if cfg!(debug) {
                for &other in &self.other_dims {
                    let order = space.domain().get_order(dim.id().into(), other.into());
                    assert_eq!(order, Order::MERGED);
                }
            }
            true
        } else {
            false
        }
    }
}

/// Creates the final list of dimensions by grouping fused `ir::Dimension`.
pub fn group_merged_dimensions<'a>(space: &'a SearchSpace) -> Vec<Dimension<'a>> {
    let mut groups: Vec<Dimension> = Vec::new();
    'dim: for dim in space.ir_instance().dims() {
        for group in &mut groups {
            if group.try_add_fused_dim(dim, space) {
                continue 'dim;
            }
        }
        groups.push(Dimension::new(dim, space));
    }
    groups
}

/// An induction level associated to a dimension.
///
/// This represents a variable in a loop that gets incremented at each iteration.
#[derive(Debug)]
pub struct InductionLevel<'a> {
    /// The identifier for the induction variable.  This is shared across the different levels that
    /// represent a given iteration.
    pub ind_var: ir::IndVarId,
    /// The increment to use, i.e. the quantity we increment the variable by every iteration.
    ///
    /// Warning: The dimension here corresponds to the IR dimension represented by this level; it
    /// does *not* have the same size as the increment!
    pub increment: Option<(ir::DimId, codegen::Size<'a>)>,
    /// The base of the induction, i.e. the value the variable is initialized to before the first
    /// iteration.
    pub base: InductionVarValue<'a>,
}

impl<'a> InductionLevel<'a> {
    /// Returns the type of the value created by the induction level.
    pub fn t(&self) -> ir::Type {
        self.base.t()
    }

    /// Returns the values to pass from the host to the device to implement `self`.
    pub fn host_values(
        &self,
        space: &SearchSpace,
    ) -> impl Iterator<Item = codegen::ParamVal<'a>> {
        self.increment
            .as_ref()
            .and_then(|&(_, ref s)| codegen::ParamVal::from_size(s))
            .into_iter()
            .chain(self.base.host_values(space))
    }
}

/// An induction variable, composed of multiple induction variable levels.
pub struct InductionVar<'a> {
    pub id: ir::IndVarId,
    pub value: InductionVarValue<'a>,
}

impl<'a> InductionVar<'a> {
    /// Returns the values to pass from the host to the device to implement `self`.
    pub fn host_values<'b>(
        &'b self,
        space: &SearchSpace,
    ) -> impl Iterator<Item = codegen::ParamVal<'a>> {
        self.value.host_values(space).into_iter()
    }
}

/// The value taken by an induction variable. The actual value is the sum of the component
/// present. If no components is present, the value must be computed elsewhere.
///
/// When the value must be computed elsewhere, it means that the value can't be directly used as an
/// operand due to a complex computation.  In that case, we will compute its value at the global
/// level (see `register_induction_vars`)
#[derive(Debug)]
pub struct InductionVarValue<'a> {
    /// The identifier for the induction variable.
    ind_var: ir::IndVarId,
    /// The outer level of the induction.
    ///
    /// This references the level nested directly outside of this one.  The value should be
    /// initialized from the corresponding variable.
    outer_level: Option<ir::DimId>,
    /// The operand from which the value should be initialized.
    operand: Option<&'a ir::Operand>,
    /// The type of the induction variable.
    t: ir::Type,
}

impl<'a> InductionVarValue<'a> {
    /// Returns the additive components of the induction variable value.
    pub fn components(&self) -> impl Iterator<Item = codegen::Operand<'a>> {
        let ind_var = self.ind_var;
        self.outer_level
            .into_iter()
            .map(move |dim| codegen::Operand::InductionLevel(ind_var, dim))
            .chain(self.operand.into_iter().map(codegen::Operand::Operand))
    }

    /// Returns the type of the value.
    pub fn t(&self) -> ir::Type {
        self.t
    }

    /// Returns and induction var value that just contains an operand.
    fn new(ind_var: ir::IndVarId, operand: &'a ir::Operand, space: &SearchSpace) -> Self {
        let t = unwrap!(space.ir_instance().device().lower_type(operand.t(), space));
        InductionVarValue {
            ind_var,
            outer_level: None,
            operand: Some(operand),
            t,
        }
    }

    /// The value is assigned elsewhere.
    fn computed_elsewhere(other: &Self) -> Self {
        let ind_var = other.ind_var;
        InductionVarValue {
            ind_var,
            outer_level: None,
            operand: None,
            t: other.t(),
        }
    }

    /// Return the current value so it can be used by a level and point the new level
    /// instead. Keep the operand if the level doesn't uses it.
    fn apply_level(&mut self, level: ir::DimId, use_operand: bool) -> Self {
        use std::mem::replace;
        InductionVarValue {
            outer_level: replace(&mut self.outer_level, Some(level)),
            operand: if use_operand {
                replace(&mut self.operand, None)
            } else {
                None
            },
            t: self.t,
            ind_var: self.ind_var,
        }
    }

    /// Returns the values to pass from the host to the device to implement `self`.
    fn host_values(&self, space: &SearchSpace) -> Option<codegen::ParamVal<'a>> {
        self.operand
            .and_then(|x| codegen::ParamVal::from_operand(x, space))
    }
}

/// Register the induction variables in the dimensions where they must be incremented.
/// Returns the induction variables and the levels to compute at the begining of the
/// kernel.
pub fn register_induction_vars<'a>(
    dims: &mut Vec<Dimension<'a>>,
    space: &'a SearchSpace,
) -> (Vec<InductionVar<'a>>, Vec<InductionLevel<'a>>) {
    let mut ind_levels_map = FxMultiHashMap::default();
    let mut ind_vars = Vec::new();
    let mut precomputed_levels = Vec::new();
    for (id, ind_var) in space.ir_instance().induction_vars() {
        let (const_levels, mut_levels) = get_ind_var_levels(ind_var, space);
        let mut outer_value = InductionVarValue::new(id, ind_var.base(), space);
        let precomputed = const_levels
            .into_iter()
            .map(|(dim, increment)| {
                let base = outer_value.apply_level(dim, false);
                InductionLevel {
                    ind_var: id,
                    increment: Some((dim, increment)),
                    base,
                }
            })
            .collect_vec();
        for (dim, increment) in mut_levels {
            let level = InductionLevel {
                ind_var: id,
                increment: Some((dim, increment)),
                base: outer_value.apply_level(dim, true),
            };
            ind_levels_map.insert(dim, level);
        }
        // If there is more than one components, the value cannot be directly used by
        // instructions.
        let value = if outer_value.components().count() > 1 {
            // FIXME(correctness): We might be unable to compute the outer value here due to
            // dependencies.  This should be an issue as this case happens only during manually
            // designed tests (?), but is still a bug.
            let value = InductionVarValue::computed_elsewhere(&outer_value);
            let level = InductionLevel {
                ind_var: id,
                increment: None,
                base: outer_value,
            };
            let dim = unwrap!(precomputed.last().and_then(|p| p.increment.as_ref())).0;
            ind_levels_map.insert(dim, level);
            value
        } else {
            outer_value
        };
        precomputed_levels.extend(precomputed);
        ind_vars.push(InductionVar { id, value });
    }
    for dim_group in dims {
        for dim_id in dim_group.dim_ids() {
            dim_group
                .induction_levels
                .extend(ind_levels_map.remove(&dim_id));
        }
    }
    (ind_vars, precomputed_levels)
}

type IndVarIncrement<'a> = (ir::DimId, codegen::Size<'a>);

/// Retrieves the list of induction levels that can be computed at the beginning of the
/// thread and the induction levels that are updated during loops. Both lists are sorted
/// in the order in which levels should be computed.
fn get_ind_var_levels<'a>(
    ind_var: &'a ir::InductionVar,
    space: &SearchSpace,
) -> (Vec<IndVarIncrement<'a>>, Vec<IndVarIncrement<'a>>) {
    let (mut const_levels, mut mut_levels) = (Vec::new(), Vec::new());
    for &(dim, ref size) in ind_var.dims() {
        let size = codegen::Size::from_ir(size, space);
        match space.domain().get_dim_kind(dim) {
            DimKind::INNER_VECTOR | DimKind::OUTER_VECTOR => (),
            DimKind::LOOP | DimKind::UNROLL => mut_levels.push((dim, size)),
            DimKind::BLOCK | DimKind::THREAD => const_levels.push((dim, size)),
            x => panic!("unspecified dim kind {:?}", x),
        }
    }
    let cmp = |lhs: ir::DimId, rhs: ir::DimId| {
        if lhs == rhs {
            return std::cmp::Ordering::Equal;
        }
        match space.domain().get_order(lhs.into(), rhs.into()) {
            Order::INNER => std::cmp::Ordering::Greater,
            Order::OUTER => std::cmp::Ordering::Less,
            Order::MERGED => std::cmp::Ordering::Equal,
            _ => panic!("invalid order for induction variable dimensions"),
        }
    };
    const_levels.sort_unstable_by(|lhs, rhs| cmp(lhs.0, rhs.0));
    mut_levels.sort_unstable_by(|lhs, rhs| cmp(lhs.0, rhs.0));
    (const_levels, mut_levels)
}
