//! Levels of the latency graph.
//!
//! A level is akin to a loop, with a body and an iteration count, but it can represent the
//! intersection of the body of multiple iteration dimensions. This is necessary when the
//! nesting order of two loops of sizes N and M is left unspecified. Indeed, when
//! considering the loops independently, we can only consider the body is repeated n or m
//! times while when considering the intersection of the bodies, we consider it is
//! repeated N x M times.

use device::Device;
use ir;
use itertools::{self, Itertools};
use model::{FastBound, LocalInfo, HwPressure, DependencyMap, BottleneckLevel};
use search_space::{DimKind, Domain, SearchSpace};
use std;
use std::cmp::Ordering;
use utils::*;

/// A level at which latency should be computed.
#[derive(Debug)]
pub struct Level {
    /// The dimensions the level iterates on.
    pub dims: VecSet<ir::dim::Id>,
    /// The latency of a single iteration of the level.
    pub latency: FastBound,
    /// The latency overhead at the end of each iteration.
    pub end_latency: FastBound,
    /// Dependencies between iterations of the level. A dependency is represented by
    /// the code point ID where the edge originates and goes to and by its latency.
    pub back_edges: Vec<(usize, FastBound)>,
    /// The latency of all the iterations of the level.
    pub repeated_latency: Option<FastBound>,
}

impl Level {
    /// Creates a level iterating on the given dimensions. If no dimension is given,
    /// the level containts the whole program.
    fn new(device: &Device,
           space: &SearchSpace,
           local_info: &LocalInfo,
           dims: VecSet<ir::dim::Id>) -> Self {
        // Compute the thread-level pressure.
        let thread_rates = device.thread_rates();
        let pressure = sum_pressure(
            device, space, local_info, BottleneckLevel::Thread, 1, &dims);
        let end_latency = dims.iter().map(|d| {
            local_info.dim_overhead[d].1.bound(BottleneckLevel::Thread, &thread_rates)
        }).min().unwrap_or_else(FastBound::zero);
        let latency = pressure.bound(BottleneckLevel::Thread, &thread_rates);
        // Compute the block-level pressure.
        let only_threads = dims.iter().all(|&d| {
            DimKind::THREAD.contains(space.domain().get_dim_kind(d))
        });
        let repeated_latency = if only_threads {
            Some(block_bound(device, space, local_info, &dims))
        } else { None };
        Level { dims, latency, end_latency, back_edges: vec![], repeated_latency }
    }
}

/// Computes the `HwPressure` caused by the intersection of the bodies of the given loops.
pub fn sum_pressure(device: &Device,
                    space: &SearchSpace,
                    local_info: &LocalInfo,
                    bound_level: BottleneckLevel,
                    min_num_threads: u64,
                    dims: &[ir::dim::Id]) -> HwPressure {
    // Compute the pressure induced by the dimensions overhead.
    let mut pressure = HwPressure::min(dims.iter().map(|d| &local_info.dim_overhead[d].0))
        .unwrap_or_else(|| HwPressure::zero(device));
    if bound_level == BottleneckLevel::Global {
        // FIXME:Take min_num_thread from the bound level instead of the argument
        let thread_overhead = &local_info.thread_overhead;
        pressure.repeat_and_add_bottlenecks(min_num_threads as f64, thread_overhead);
    }
    // Get the list of inner dimensions and inner dimensions on wich the pressure is summed.
    let inner_dim_sets = dims.iter().map(|&d| &local_info.nesting[&d.into()].inner_dims);
    let inner_dims = intersect_sets(inner_dim_sets).unwrap_or_else(|| {
        space.ir_instance().dims().map(|d| d.id()).collect()
    });
    let inner_sum_dims = inner_dims.filter(|&d| {
        bound_level.accounts_for_dim(space.domain().get_dim_kind(d))
    });
    // Get the list of inner basic blocks.
    let inner_bbs_sets = dims.iter().map(|&d| &local_info.nesting[&d.into()].inner_bbs);
    let inner_bbs = intersect_sets(inner_bbs_sets)
        .map(|x| itertools::Either::Left(x.into_iter()))
        .unwrap_or_else(|| {
            itertools::Either::Right(space.ir_instance().blocks().map(|bb| bb.bb_id()))
        });
    // Sum the pressure on all bbs.
    for bb in inner_bbs {
        // Skip dimensions that can be merged into another one.
        let merge_dims = &local_info.nesting[&bb].bigger_merged_dims;
        if inner_dims.intersection(merge_dims).next().is_some() { continue; }
        // Compute the pressure of a single instance and the number of instances.
        let mut num_instances = inner_sum_dims
            .intersection(&local_info.nesting[&bb].outer_dims)
            .map(|d| f64::from(local_info.dim_sizes[d]))
            .product::<f64>();
        let bb_pressure = if let ir::BBId::Dim(dim) = bb {
            let kind = space.domain().get_dim_kind(dim);
            if !bound_level.accounts_for_dim(kind) {
                &local_info.dim_overhead[&dim].0
            } else { &local_info.hw_pressure[&bb] }
        } else { &local_info.hw_pressure[&bb] };
        // Predicated instructions are not executed on unmapped thread dimensions.
        let is_predicated = space.ir_instance().block(bb).as_inst()
            .map(|i| i.has_side_effects()).unwrap_or(false);
        let unmapped_threads = local_info.nesting[&bb].num_unmapped_threads as f64;
        if bound_level <= BottleneckLevel::Block {
            if is_predicated {
                let num_skipped = unmapped_threads * (num_instances - 1.0);
                pressure.repeat_and_add_bottlenecks(num_skipped, &device.skipped_pressure());
            } else {
                num_instances *= unmapped_threads;
            }
        }
        pressure.repeat_and_add_bottlenecks(num_instances, bb_pressure);
    }
    pressure
}

/// Computes the intersection of several `VecSet`.
fn intersect_sets<'a, T, IT>(mut it: IT) -> Option<VecSet<T>>
    where IT: Iterator<Item=&'a VecSet<T>>, T: std::cmp::Ord + Clone + 'a
{
    it.next().map(|out| {
        let mut out = out.clone();
        for other in it { out.intersect(other); }
        out
    })
}

/// Generates a bound based on the pressure produced by a block of threads.
fn block_bound(device: &Device, space: &SearchSpace, info: &LocalInfo,
               dims: &[ir::dim::Id]) -> FastBound {
    // Compute the minimal and maximal number of threads.
    let min_num_threads = info.parallelism.min_num_threads_per_block;
    let max_num_threads = info.parallelism.max_num_threads_per_block;
    // Compute the pressure on the execution units in a single iteration.
    let mut pressure = sum_pressure(device, space, info, BottleneckLevel::Block,
                                    min_num_threads, dims);
    // Repeat the pressure by the number of iterations of the level and compute the bound.
    let n_iters = dims.iter().map(|&d| u64::from(info.dim_sizes[&d])).product::<u64>();
    pressure.repeat_parallel(n_iters as f64);
    pressure.bound(BottleneckLevel::Block, &device.block_rates(max_num_threads))
}

/// Indicates if a dimension should be considered for dimension levels.
pub fn must_consider_dim(space :&SearchSpace, dim: ir::dim::Id) -> bool {
    let kind = space.domain().get_dim_kind(dim);
    kind != DimKind::BLOCK && !kind.intersects(DimKind::VECTOR)
}

/// Generates the list of levels to consider. The root level is the first one.
///
/// The idea is to ensure that each instruction is considered the right number of times
/// and that inner loops are applied before outer ones. For this, we build the list of
/// outer dimensions of each instruction or loops. For loops, we include both the nesting
/// with and without the loop.  We then build the minimal dag for the order defined such as
/// X < Y iff:
/// - nesting(X) < nesting(Y)
/// - forall z in Y\X, forall y in Y, z inner y
/// Each edge of the dag represents a level, appling the dimensions in the difference
/// between the nestings at each end of the edge.
pub fn generate(space: &SearchSpace, device: &Device,
                local_info: &LocalInfo) -> (Vec<Level>, Vec<DimMap>) {
    // Build the list of nestings, exclude block and vector dimensions.
    let mut nestings = local_info.nesting.iter().flat_map(|(&bb, nesting)| {
        let outer_dims = nesting.outer_dims.filter(|&d| must_consider_dim(space, d));
        if let ir::BBId::Dim(dim) = bb {
            if must_consider_dim(space, dim) {
                let mut outer_with_self = outer_dims.clone();
                outer_with_self.insert(dim);
                vec![outer_dims, outer_with_self]
            } else { vec![] }
        } else { vec![outer_dims] }
    }).collect_vec();
    let dim_maps = list_dim_maps(space);
    // Add the nesting of dim maps
    for dim_map in &dim_maps {
        let outer_dims = dim_map.src_dims.iter().chain(&dim_map.dst_dims)
            .map(|&d| &local_info.nesting[&d.into()].outer_dims);
        nestings.push(unwrap!(intersect_sets(outer_dims)));
    }
    // Build the DAG from nestings.
    let dag = Dag::from_order(nestings, |lhs, rhs| {
        match lhs.partial_cmp(rhs) {
            Some(Ordering::Less) => {
                let diff: VecSet<_> = rhs.difference(lhs).cloned().collect();
                if lhs.iter().all(|&x| local_info.nesting[&x.into()].inner_dims >= diff) {
                    Some(Ordering::Less)
                } else { None }
            },
            Some(Ordering::Greater) => {
                let diff: VecSet<_> = lhs.difference(rhs).cloned().collect();
                if rhs.iter().all(|&x| local_info.nesting[&x.into()].inner_dims >= diff) {
                    Some(Ordering::Greater)
                } else { None }
            },
            x => x,
        }
    });
    // Retrieve loop levels.
    let dim_levels = dag.nodes().iter().enumerate().flat_map(|(start_id, start)| {
        let nodes = dag.nodes();
        dag.after(start_id).iter().map(move |&end_id| {
            nodes[end_id].difference(start).cloned().collect::<VecSet<_>>()
        })
    }).flat_map(|dims| {
        // We only need to keep the sequential part of multi-dim levels as they are only
        // needed to iterate on the dimensions.
        if dims.len() <= 1 { Some(dims) }
        else {
            let sequential = dims.into_iter().filter(|&d| {
                let kind = space.domain().get_dim_kind(d);
                (kind & !DimKind::BLOCK).is(DimKind::SEQUENTIAL).is_true()
            }).collect::<VecSet<_>>();
            if sequential.is_empty() { None } else { Some(sequential) }
        }
    });
    let levels = std::iter::once(VecSet::default()).chain(dim_levels).unique();
    let levels = levels.map(|dims| Level::new(device, space, local_info, dims)).collect();
    (levels, dim_maps)
}

/// A dim-map that must be accounted for.
#[derive(Debug)]
pub struct DimMap {
    pub src: ir::InstId,
    pub dst: ir::InstId,
    pub src_dims: VecSet<ir::dim::Id>,
    pub dst_dims: VecSet<ir::dim::Id>,
}

/// Lists the dim maps that must be considered by the performance model.
fn list_dim_maps(space: &SearchSpace) -> Vec<DimMap> {
    space.ir_instance().insts().flat_map(|inst| {
        let dst = inst.id();
        inst.operands().into_iter().flat_map(move |op| match *op {
            ir::Operand::Inst(src, _, ref dim_map, _) |
            ir::Operand::Reduce(src, _, ref dim_map, _) => {
                let src_dims = dim_map.iter().map(|x| x.0)
                    .filter(|&d| must_consider_dim(space, d))
                    .collect::<VecSet<_>>();
                let dst_dims = dim_map.iter().map(|x| x.1)
                    .filter(|&d| must_consider_dim(space, d))
                    .collect::<VecSet<_>>();
                if dst_dims.is_empty() || src_dims.is_empty() {
                    None
                } else {
                    Some(DimMap { src, dst, src_dims, dst_dims })
                }
            },
            _ => None,
        })
    }).collect()
}

/// Indicates how a the sequential dimensions of a level should be repeated in the latency
/// graph.
#[derive(Copy, Clone, Debug)]
pub struct RepeatLevel {
    /// The ID of the level to repeat.
    pub level_id: usize,
    /// The number of iterations of the level.
    pub iterations: u32,
}

impl RepeatLevel {
    pub fn new(space: &SearchSpace,
               local_info: &LocalInfo,
               level_id: usize,
               level: &Level) -> Option<Self> {
        let iterations: u32 = level.dims.iter().filter(|&&d| {
            let kind = space.domain().get_dim_kind(d);
            (kind & !DimKind::BLOCK).is(DimKind::SEQUENTIAL).is_true()
        }).map(|d| local_info.dim_sizes[d]).product();
        if iterations <= 1 { None } else {
            Some(RepeatLevel { level_id, iterations })
        }
    }
}

/// Exposes the levels application order.
#[derive(Debug)]
pub struct LevelDag {
    node_ids: HashMap<VecSet<ir::dim::Id>, usize>,
    nodes: Vec<(Vec<RepeatLevel>, Vec<DimMap>, DependencyMap)>,
}

/// Identifies a node of the `LevelDag`.
#[derive(Copy, Clone, Debug)]
pub struct DagNodeId(usize);

impl LevelDag {
    /// Creates and empty `LevelDag`, with only the root node.
    fn new(space: &SearchSpace, dep_map_size: usize) -> Self {
        let mut node_ids = HashMap::default();
        let all_dims = space.ir_instance().dims().map(|d| d.id())
            .filter(|&d| space.domain().get_dim_kind(d) != DimKind::BLOCK)
            .collect();
        node_ids.insert(all_dims, 0);
        LevelDag {
            node_ids,
            nodes: vec![(vec![], vec![], DependencyMap::new(dep_map_size))],
        }
    }

    /// Generates the `LevelDag` for the given levels.
    pub fn build(space: &SearchSpace, local_info: &LocalInfo, levels: &[Level],
                 dim_maps: Vec<DimMap>, dep_map_size: usize) -> Self {
        let mut dag = LevelDag::new(space, dep_map_size);
        for (level_id, level) in levels.iter().enumerate() {
            if level.dims.is_empty() { continue; }
            let node_id = dag.gen_node_id(local_info, &level.dims, dep_map_size);
            let repeat = RepeatLevel::new(space, local_info, level_id, level);
            dag.nodes[node_id].0.extend(repeat);
        }
        for dim_map in dim_maps {
            let node_id = dag.gen_node_id(local_info, &dim_map.src_dims, dep_map_size);
            dag.nodes[node_id].1.push(dim_map);
        }
        dag
    }

    fn gen_node_id(&mut self, local_info: &LocalInfo, level_dims: &[ir::dim::Id],
                   dep_map_size: usize) -> usize {
        let before = level_dims.iter().map(|&d| {
            &local_info.nesting[&d.into()].before_self
        });
        let nodes = &mut self.nodes;
        *self.node_ids.entry(unwrap!(intersect_sets(before))).or_insert_with(|| {
            nodes.push((vec![], vec![], DependencyMap::new(dep_map_size)));
            nodes.len()-1
        })
    }

    /// Adds a dependency to all nodes that where the given dimensions are processed.
    pub fn add_if_processed(&mut self, dims: &VecSet<ir::dim::Id>,
                            dep_start: usize, dep_end: usize, dep_lat: &FastBound) {
        for (node_dims, &node_id) in &self.node_ids {
            if dims <= node_dims {
                self.nodes[node_id].2.add_dep(dep_start, dep_end, dep_lat.clone());
            }
        }
    }

    /// Adds a dependency to all the give nodes.
    pub fn add_dependency(&mut self, nodes: &[DagNodeId],
                          dep_start: usize, dep_end: usize, dep_lat: &FastBound) {
        for &node_id in nodes {
            self.nodes[node_id.0].2.add_dep(dep_start, dep_end, dep_lat.clone());
        }
    }

    /// Adds a dependency to all the nodes.
    pub fn add_dependency_to_all(&mut self, dep_start: usize,
                                 dep_end: usize, dep_lat: &FastBound) {
        for &mut (_, _, ref mut dep_map) in &mut self.nodes {
            dep_map.add_dep(dep_start, dep_end, dep_lat.clone());
        }
    }

    /// Gets the dependency maps associated to a given node.
    pub fn dependencies(&self, node: DagNodeId) -> &DependencyMap { &self.nodes[node.0].2 }

    /// Return the list of dag nodes, sorted by processing order.
    pub fn processing_order(&mut self, levels: &[Level])
        -> Vec<(DagNodeId, DagAction, Vec<DagNodeId>)>
    {
        let mut actions = Vec::new();
        // Create the list of actions.
        for (dims, &from) in &self.node_ids {
            // Find the nodes that are after the origin.
            let nodes_after = self.node_ids.iter()
                .filter(|&(after_dims, _)| after_dims >= dims)
                .collect_vec();
            // Create the level repreat actions.
            for repeat in self.nodes[from].0.drain(..) {
                let dims = &levels[repeat.level_id].dims;
                let nodes_after = nodes_after.iter().cloned()
                    .filter(|&(after_dims, _)| after_dims >= dims)
                    .map(|(_, &id)| DagNodeId(id)).collect_vec();
                actions.push((DagNodeId(from), DagAction::Repeat(repeat), nodes_after));
            }
            // Create the actions for dim maps.
            for dim_map in self.nodes[from].1.drain(..) {
                let nodes_after = nodes_after.iter().cloned()
                    .filter(|&(after_dims, _)| after_dims >= &dim_map.src_dims)
                    .map(|(_, &id)| DagNodeId(id)).collect_vec();
                let action = DagAction::ApplyDimMap(dim_map);
                actions.push((DagNodeId(from), action, nodes_after));
            }
        }
        // Sort the list by the reverse number of to_nodes.
        actions.sort_by(|lhs, rhs| lhs.2.len().cmp(&rhs.2.len()).reverse());
        actions
    }

    /// Returns the root of the `LevelDag`.
    pub fn root(&self) -> &DependencyMap { &self.nodes[0].2 }
}

/// An action to perform on the `LevelDag`.
pub enum DagAction { Repeat(RepeatLevel), ApplyDimMap(DimMap) }
