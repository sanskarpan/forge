//! Runtime SIMD capability selection shared by native front ends.

use forge_ir::array::ArrayParamKind;
use forge_ir::{CmpOp, Function, Inst, Terminator, Ty, Value};

/// The typed, lane-wise IR consumed by the packed evaluator. Values reuse the
/// scalar IR's SSA indices so the vector program can be inspected alongside
/// the scalar pipeline without a second value-numbering scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VectorInst {
    SplatF64(u64),
    Param {
        index: u32,
    },
    Move(Value),
    Add(Value, Value),
    Sub(Value, Value),
    Mul(Value, Value),
    Div(Value, Value),
    Min(Value, Value),
    Max(Value, Value),
    Neg(Value),
    Sqrt(Value),
    Abs(Value),
    Floor(Value),
    Ceil(Value),
    Round(Value),
    Trunc(Value),
    Fma {
        a: Value,
        b: Value,
        c: Value,
    },
    VecLoad {
        base: Value,
        offset: i32,
        lanes: u8,
    },
    VecStore {
        base: Value,
        offset: i32,
        value: Value,
        lanes: u8,
    },
    VecReduce {
        op: ReduceOp,
        vec: Value,
    },
    Cmp {
        op: CmpOp,
        lhs: Value,
        rhs: Value,
    },
    Select {
        cond: Value,
        then_: Value,
        else_: Value,
    },
    MaskAnd(Value, Value),
    MaskOr(Value, Value),
    MaskXor(Value, Value),
    MaskNot(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReduceOp {
    Sum,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorFunction {
    pub lanes: u8,
    pub insts: Vec<(Value, VectorInst)>,
    pub result: Value,
}

/// The explicit loop envelope used by array mode. The body remains a typed
/// SSA dataflow graph, while this small control-flow layer owns the induction
/// variable and the scalar tail boundary. Keeping the loop plan separate from
/// the scalar IR makes it possible to reuse one lowered body for every full
/// chunk instead of rebuilding it for each input offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VectorStore {
    pub value: Value,
    pub offset: i32,
    pub lanes: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorLoop {
    pub lanes: u8,
    pub elements: usize,
    pub induction_start: usize,
    pub induction_step: usize,
    pub full_chunks: usize,
    pub tail: usize,
    pub body: VectorFunction,
    pub store: VectorStore,
}

/// Lowers the supported scalar subset into typed vector IR.
/// Scalar f64 parameters become explicit `VecLoad` operations. Pure structured
/// control flow is converted to lane masks and predicated selects; calls,
/// integer values, and unsupported operations still reject the packed path.
pub fn lower_f64_vector(function: &Function, lanes: u8) -> Result<VectorFunction, String> {
    if !matches!(lanes, 2 | 4 | 8) {
        return Err(format!("unsupported f64 vector width: {lanes}"));
    }
    if function.blocks.len() == 1 {
        let block = &function.blocks[0];
        let Some(Terminator::Return(result)) = block.term.as_ref() else {
            return Err("vector lowering requires a return terminator".to_string());
        };
        let mut insts = Vec::with_capacity(block.insts.len());
        for &value in &block.insts {
            let scalar = function
                .insts
                .get(value.0 as usize)
                .ok_or_else(|| format!("block references missing instruction {value:?}"))?;
            insts.push((value, lower_vector_inst(function, value, scalar, lanes)?));
        }
        return Ok(VectorFunction {
            lanes,
            insts,
            result: *result,
        });
    }

    lower_structured_vector(function, lanes)
}

fn lower_vector_inst(
    function: &Function,
    value: Value,
    scalar: &Inst,
    lanes: u8,
) -> Result<VectorInst, String> {
    let result_ty = function
        .types
        .get(value.0 as usize)
        .copied()
        .ok_or_else(|| format!("missing type for vector value {value:?}"))?;
    let vector = match scalar {
        Inst::ConstF64(bits) => VectorInst::SplatF64(*bits),
        Inst::ConstI64(number) => VectorInst::SplatF64((*number as f64).to_bits()),
        Inst::ConstBool(value) => {
            VectorInst::SplatF64((if *value { 1.0f64 } else { 0.0f64 }).to_bits())
        }
        Inst::Param { ty: Ty::F64, .. } => VectorInst::VecLoad {
            base: value,
            offset: 0,
            lanes,
        },
        Inst::Add(lhs, rhs) => VectorInst::Add(*lhs, *rhs),
        Inst::Sub(lhs, rhs) => VectorInst::Sub(*lhs, *rhs),
        Inst::Mul(lhs, rhs) => VectorInst::Mul(*lhs, *rhs),
        Inst::Div(lhs, rhs) => VectorInst::Div(*lhs, *rhs),
        Inst::Min(lhs, rhs) => VectorInst::Min(*lhs, *rhs),
        Inst::Max(lhs, rhs) => VectorInst::Max(*lhs, *rhs),
        Inst::Neg(operand) => VectorInst::Neg(*operand),
        Inst::Sqrt(operand) => VectorInst::Sqrt(*operand),
        Inst::Abs(operand) => VectorInst::Abs(*operand),
        Inst::Floor(operand) => VectorInst::Floor(*operand),
        Inst::Ceil(operand) => VectorInst::Ceil(*operand),
        Inst::Round(operand) => VectorInst::Round(*operand),
        Inst::Trunc(operand) => VectorInst::Trunc(*operand),
        Inst::Fma { a, b, c } => VectorInst::Fma {
            a: *a,
            b: *b,
            c: *c,
        },
        Inst::IToF(operand) => VectorInst::Move(*operand),
        Inst::Cmp { op, lhs, rhs }
            if result_ty == Ty::Bool
                && function.types.get(lhs.0 as usize) == Some(&Ty::F64)
                && function.types.get(rhs.0 as usize) == Some(&Ty::F64) =>
        {
            VectorInst::Cmp {
                op: *op,
                lhs: *lhs,
                rhs: *rhs,
            }
        }
        Inst::And(lhs, rhs) if result_ty == Ty::Bool => VectorInst::MaskAnd(*lhs, *rhs),
        Inst::Or(lhs, rhs) if result_ty == Ty::Bool => VectorInst::MaskOr(*lhs, *rhs),
        Inst::Xor(lhs, rhs) if result_ty == Ty::Bool => VectorInst::MaskXor(*lhs, *rhs),
        Inst::Not(operand) if result_ty == Ty::Bool => VectorInst::MaskNot(*operand),
        Inst::Param { .. } => return Err("vector lowering requires f64 parameters".to_string()),
        _ => {
            return Err(format!(
                "scalar instruction is not vectorizable: {scalar:?}"
            ))
        }
    };
    Ok(vector)
}

fn reachable_blocks(function: &Function) -> Result<Vec<usize>, String> {
    fn visit(
        function: &Function,
        block: usize,
        marks: &mut [u8],
        postorder: &mut Vec<usize>,
    ) -> Result<(), String> {
        match marks.get(block).copied() {
            Some(1) => return Err("vector lowering does not support CFG loops".to_string()),
            Some(2) => return Ok(()),
            None => return Err(format!("CFG references missing block {block}")),
            _ => {}
        }
        marks[block] = 1;
        match function.blocks[block].term.as_ref() {
            Some(Terminator::Branch { then_, else_, .. }) => {
                visit(function, then_.0 as usize, marks, postorder)?;
                visit(function, else_.0 as usize, marks, postorder)?;
            }
            Some(Terminator::Jump(target)) => {
                visit(function, target.0 as usize, marks, postorder)?;
            }
            Some(Terminator::Return(_)) => {}
            None => return Err(format!("CFG block {block} has no terminator")),
        }
        marks[block] = 2;
        postorder.push(block);
        Ok(())
    }

    let mut marks = vec![0; function.blocks.len()];
    let mut postorder = Vec::new();
    visit(
        function,
        function.entry.0 as usize,
        &mut marks,
        &mut postorder,
    )?;
    postorder.reverse();
    Ok(postorder)
}

fn lower_structured_vector(function: &Function, lanes: u8) -> Result<VectorFunction, String> {
    let order = reachable_blocks(function)?;
    let mut insts = Vec::new();
    let mut next_aux = function.insts.len() as u32;
    let mut fresh = |inst: VectorInst, output: &mut Vec<(Value, VectorInst)>| {
        let value = Value(next_aux);
        next_aux += 1;
        output.push((value, inst));
        value
    };
    let true_mask = fresh(VectorInst::SplatF64(1.0f64.to_bits()), &mut insts);
    let mut paths = vec![None; function.blocks.len()];
    paths[function.entry.0 as usize] = Some(true_mask);
    let mut result = None;

    for block_index in order {
        let path = paths[block_index]
            .ok_or_else(|| format!("missing vector path mask for block {block_index}"))?;
        let block = &function.blocks[block_index];
        for &value in &block.insts {
            let scalar = function
                .insts
                .get(value.0 as usize)
                .ok_or_else(|| format!("block references missing instruction {value:?}"))?;
            if let Inst::Phi { incoming } = scalar {
                let Some(&(first_block, first_value)) = incoming.first() else {
                    return Err("vector lowering rejects an empty phi".to_string());
                };
                paths
                    .get(first_block.0 as usize)
                    .and_then(|path| *path)
                    .ok_or_else(|| "missing vector path for phi predecessor".to_string())?;
                let mut selected = first_value;
                for (position, &(incoming_block, incoming_value)) in
                    incoming.iter().skip(1).enumerate()
                {
                    let incoming_path = paths
                        .get(incoming_block.0 as usize)
                        .and_then(|path| *path)
                        .ok_or_else(|| "missing vector path for phi predecessor".to_string())?;
                    let final_incoming = position + 2 == incoming.len();
                    let output = if final_incoming {
                        value
                    } else {
                        fresh(
                            VectorInst::Select {
                                cond: incoming_path,
                                then_: incoming_value,
                                else_: selected,
                            },
                            &mut insts,
                        )
                    };
                    if final_incoming {
                        insts.push((
                            output,
                            VectorInst::Select {
                                cond: incoming_path,
                                then_: incoming_value,
                                else_: selected,
                            },
                        ));
                    }
                    selected = output;
                }
                if incoming.len() == 1 {
                    insts.push((value, VectorInst::Move(first_value)));
                }
            } else {
                insts.push((value, lower_vector_inst(function, value, scalar, lanes)?));
            }
        }

        match block.term.as_ref() {
            Some(Terminator::Return(value)) => result = Some(*value),
            Some(Terminator::Jump(target)) => {
                merge_path(&mut paths[target.0 as usize], path, &mut fresh, &mut insts)
            }
            Some(Terminator::Branch { cond, then_, else_ }) => {
                let then_path = fresh(VectorInst::MaskAnd(path, *cond), &mut insts);
                let not_cond = fresh(VectorInst::MaskNot(*cond), &mut insts);
                let else_path = fresh(VectorInst::MaskAnd(path, not_cond), &mut insts);
                merge_path(
                    &mut paths[then_.0 as usize],
                    then_path,
                    &mut fresh,
                    &mut insts,
                );
                merge_path(
                    &mut paths[else_.0 as usize],
                    else_path,
                    &mut fresh,
                    &mut insts,
                );
            }
            None => return Err(format!("CFG block {block_index} has no terminator")),
        }
    }

    Ok(VectorFunction {
        lanes,
        insts,
        result: result.ok_or_else(|| "vector lowering found no return".to_string())?,
    })
}

fn merge_path(
    slot: &mut Option<Value>,
    incoming: Value,
    fresh: &mut impl FnMut(VectorInst, &mut Vec<(Value, VectorInst)>) -> Value,
    insts: &mut Vec<(Value, VectorInst)>,
) {
    *slot = Some(match *slot {
        Some(existing) => fresh(VectorInst::MaskOr(existing, incoming), insts),
        None => incoming,
    });
}

/// Lowers a scalar all-f64 function into an explicit packed loop. The loop
/// performs `full_chunks` iterations at offsets
/// `induction_start + iteration * induction_step`; incomplete elements are
/// deliberately left to the caller's scalar epilogue or masked-tail path.
pub fn lower_f64_vector_loop(
    function: &Function,
    lanes: u8,
    elements: usize,
) -> Result<VectorLoop, String> {
    if !matches!(lanes, 2 | 4 | 8) {
        return Err(format!("unsupported f64 vector width: {lanes}"));
    }
    let body = lower_f64_vector(function, lanes)?;
    let induction_step = usize::from(lanes);
    Ok(VectorLoop {
        lanes,
        elements,
        induction_start: 0,
        induction_step,
        full_chunks: elements / induction_step,
        tail: elements % induction_step,
        store: VectorStore {
            value: body.result,
            offset: 0,
            lanes,
        },
        body,
    })
}

fn evaluate_scalar(source: &str, args: &[f64]) -> Result<f64, String> {
    let values = args
        .iter()
        .copied()
        .map(forge_ir::interp::RtValue::F64)
        .collect::<Vec<_>>();
    match forge_runtime::interpret_source(source, &values).map_err(|error| error.to_string())? {
        forge_ir::interp::RtValue::F64(value) => Ok(value),
        _ => Err("array evaluation expression did not return f64".to_string()),
    }
}

/// CPU capabilities sampled when a compilation starts. The JIT uses a
/// snapshot instead of compiling ISA assumptions into the portable frontend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuFeatures {
    pub sse2: bool,
    pub sse41: bool,
    pub avx: bool,
    pub avx2: bool,
    pub fma: bool,
    pub avx512f: bool,
    pub avx512dq: bool,
    pub bmi2: bool,
    pub neon: bool,
    pub sve: bool,
}

impl CpuFeatures {
    pub const fn scalar() -> Self {
        Self {
            sse2: false,
            sse41: false,
            avx: false,
            avx2: false,
            fma: false,
            avx512f: false,
            avx512dq: false,
            bmi2: false,
            neon: false,
            sve: false,
        }
    }

    pub fn detect() -> Self {
        detect_impl()
    }

    /// Intersects this requested mask with the capabilities of the current
    /// host. This is deliberately separate from [`CpuFeatures::best_width`]:
    /// width selection must never turn a caller-provided mask into an unsafe
    /// ISA claim.
    pub fn supported_by_host(self) -> Self {
        let host = Self::detect();
        Self {
            sse2: self.sse2 && host.sse2,
            sse41: self.sse41 && host.sse41,
            avx: self.avx && host.avx,
            avx2: self.avx2 && host.avx2,
            fma: self.fma && host.fma,
            avx512f: self.avx512f && host.avx512f,
            avx512dq: self.avx512dq && host.avx512dq,
            bmi2: self.bmi2 && host.bmi2,
            neon: self.neon && host.neon,
            sve: self.sve && host.sve,
        }
    }

    pub fn best_width(self, ty: Ty) -> u8 {
        match ty {
            Ty::F64 if self.avx512f => 8,
            Ty::F64 if self.avx2 => 4,
            Ty::F64 if self.sse2 || self.neon => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdWidth {
    Scalar,
    F64x2,
    F64x4,
    F64x8,
}

impl SimdWidth {
    pub const fn lanes(self) -> usize {
        match self {
            Self::Scalar => 1,
            Self::F64x2 => 2,
            Self::F64x4 => 4,
            Self::F64x8 => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrayPlan {
    pub width: SimdWidth,
    pub elements: usize,
    pub full_chunks: usize,
    pub tail: usize,
}

impl ArrayPlan {
    pub fn for_len(elements: usize) -> Self {
        Self::for_len_with_features(elements, CpuFeatures::detect())
    }

    pub fn for_len_with_features(elements: usize, features: CpuFeatures) -> Self {
        let width = match features.best_width(Ty::F64) {
            8 => SimdWidth::F64x8,
            4 => SimdWidth::F64x4,
            2 => SimdWidth::F64x2,
            _ => SimdWidth::Scalar,
        };
        let lanes = width.lanes();
        Self {
            width,
            elements,
            full_chunks: elements / lanes,
            tail: elements % lanes,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ArrayResult {
    pub values: Vec<f64>,
    pub plan: ArrayPlan,
    /// True only when the typed vector IR and a host ISA implementation
    /// successfully evaluate the packed chunks, including an AVX-512 masked
    /// tail when one is present.
    pub used_packed_backend: bool,
}

#[derive(Debug, PartialEq)]
pub struct ReductionResult {
    pub value: f64,
    pub plan: ArrayPlan,
    pub used_packed_backend: bool,
}

#[derive(Clone, Copy, Debug)]
enum ArrayInput<'a> {
    Column(&'a [f64]),
    Scalar(f64),
}

impl ArrayInput<'_> {
    fn value_at(self, index: usize) -> f64 {
        match self {
            Self::Column(column) => column[index],
            Self::Scalar(value) => value,
        }
    }
}

fn prepare_array(
    source: &str,
    columns: &[&[f64]],
    features: CpuFeatures,
) -> Result<(Function, ArrayPlan), String> {
    let function = forge_runtime::lower_source(source).map_err(|error| error.to_string())?;
    if function.params.len() != columns.len()
        || function.params.iter().any(|(_, ty)| *ty != Ty::F64)
        || function.types.last() != Some(&Ty::F64)
    {
        return Err(
            "array evaluation requires an all-f64 expression and one column per parameter"
                .to_string(),
        );
    }
    let elements = columns.first().map_or(0, |column| column.len());
    if columns.iter().any(|column| column.len() != elements) {
        return Err("array columns must have equal lengths".to_string());
    }
    Ok((
        function,
        ArrayPlan::for_len_with_features(elements, features),
    ))
}

/// Evaluates a pure expression over one column per free f64 parameter. Full
/// chunks use the widest safe packed backend for the host when the lowered
/// function is a straight-line or pure structured-control-flow f64 expression.
/// AVX-512 hosts also use a k-masked packed tail; other widths use the scalar
/// interpreter for a tail. The scalar interpreter remains the correctness
/// fallback for loops, libm calls, and operations whose hardware
/// NaN/rounding behavior does not exactly match the oracle.
pub fn evaluate_array(source: &str, columns: &[&[f64]]) -> Result<ArrayResult, String> {
    evaluate_array_with_features(source, columns, CpuFeatures::detect())
}

/// Evaluates an array expression after applying an explicit CPU feature mask.
/// Requested features are intersected with the host snapshot before width
/// selection, so callers can disable packed backends for testing or policy
/// reasons without accidentally enabling instructions the current CPU cannot
/// execute. `CpuFeatures::scalar()` therefore provides a deterministic,
/// bit-for-bit interpreter fallback.
pub fn evaluate_array_with_features(
    source: &str,
    columns: &[&[f64]],
    requested: CpuFeatures,
) -> Result<ArrayResult, String> {
    let features = requested.supported_by_host();
    let (function, plan) = prepare_array(source, columns, features)?;
    let inputs = columns
        .iter()
        .copied()
        .map(ArrayInput::Column)
        .collect::<Vec<_>>();
    evaluate_lowered_array_with_features(&function, &inputs, plan, features)
}

fn evaluate_lowered_array_with_features(
    function: &Function,
    inputs: &[ArrayInput<'_>],
    plan: ArrayPlan,
    features: CpuFeatures,
) -> Result<ArrayResult, String> {
    let elements = plan.elements;
    if plan.width != SimdWidth::Scalar {
        let mut values = Vec::with_capacity(elements);
        let lanes = plan.width.lanes();
        if let Some(chunks) = try_evaluate_packed_loop(function, inputs, plan, features) {
            let mut used_packed = plan.full_chunks > 0;
            values.extend(chunks);
            if plan.tail > 0 {
                if let Some(tail) = try_evaluate_packed_tail(
                    function,
                    inputs,
                    plan.full_chunks * lanes,
                    plan.width,
                    plan.tail,
                    features,
                ) {
                    values.extend(tail);
                    used_packed = true;
                } else {
                    for index in plan.full_chunks * lanes..elements {
                        let args = inputs
                            .iter()
                            .map(|input| input.value_at(index))
                            .collect::<Vec<_>>();
                        values.push(evaluate_scalar_function(function, &args)?);
                    }
                }
            }
            if used_packed && values.len() == elements {
                return Ok(ArrayResult {
                    values,
                    plan,
                    used_packed_backend: true,
                });
            }
        }
    }

    let mut values = Vec::with_capacity(elements);
    for index in 0..elements {
        let args = inputs
            .iter()
            .map(|input| input.value_at(index))
            .collect::<Vec<_>>();
        values.push(evaluate_scalar_function(function, &args)?);
    }
    Ok(ArrayResult {
        values,
        plan,
        used_packed_backend: false,
    })
}

fn evaluate_scalar_function(function: &Function, args: &[f64]) -> Result<f64, String> {
    if function.params.len() != args.len() || function.params.iter().any(|(_, ty)| *ty != Ty::F64) {
        return Err("array element function requires all-f64 parameters".to_string());
    }
    match forge_ir::interp::interpret(
        function,
        &args
            .iter()
            .copied()
            .map(forge_ir::interp::RtValue::F64)
            .collect::<Vec<_>>(),
    ) {
        forge_ir::interp::RtValue::F64(value) => Ok(value),
        _ => Err("array element function did not return f64".to_string()),
    }
}

/// Evaluates the documented source-level `@vectorize output[index] = ...`
/// form. Its array envelope is lowered to [`forge_ir::array::ArrayFunction`]
/// before the same packed loop, masked-tail, and scalar-equivalence machinery
/// used by [`evaluate_array_with_features`] is selected.
pub fn evaluate_vectorized(source: &str, columns: &[&[f64]]) -> Result<ArrayResult, String> {
    evaluate_vectorized_with_features(source, columns, CpuFeatures::detect())
}

/// Feature-masked form of [`evaluate_vectorized`]. The mask is intersected
/// with the host capabilities exactly like the existing array API.
pub fn evaluate_vectorized_with_features(
    source: &str,
    columns: &[&[f64]],
    requested: CpuFeatures,
) -> Result<ArrayResult, String> {
    evaluate_vectorized_with_broadcasts_and_features(source, columns, &[], requested)
}

/// Evaluates a source-level vectorized expression with scalar f64 broadcasts.
/// `columns` and `broadcasts` follow the declaration order of indexed and
/// unindexed parameters respectively. Broadcasts are splatted directly into
/// packed registers; no repeated input column is materialized.
pub fn evaluate_vectorized_with_broadcasts(
    source: &str,
    columns: &[&[f64]],
    broadcasts: &[f64],
) -> Result<ArrayResult, String> {
    evaluate_vectorized_with_broadcasts_and_features(
        source,
        columns,
        broadcasts,
        CpuFeatures::detect(),
    )
}

/// Feature-masked form of [`evaluate_vectorized_with_broadcasts`].
pub fn evaluate_vectorized_with_broadcasts_and_features(
    source: &str,
    columns: &[&[f64]],
    broadcasts: &[f64],
    requested: CpuFeatures,
) -> Result<ArrayResult, String> {
    let array_function =
        forge_runtime::lower_array_source(source).map_err(|error| error.to_string())?;
    let column_count = array_function
        .param_kinds
        .iter()
        .filter(|kind| **kind == ArrayParamKind::Column)
        .count();
    let broadcast_count = array_function
        .param_kinds
        .iter()
        .filter(|kind| **kind == ArrayParamKind::ScalarBroadcast)
        .count();
    if column_count != columns.len() || broadcast_count != broadcasts.len() {
        return Err("vectorized columns and broadcasts must match the input signature".to_string());
    }
    let elements = columns.first().map_or(0, |column| column.len());
    if columns.iter().any(|column| column.len() != elements) {
        return Err("vectorized columns must have equal lengths".to_string());
    }
    let mut column = 0usize;
    let mut broadcast = 0usize;
    let inputs = array_function
        .param_kinds
        .iter()
        .map(|kind| match kind {
            ArrayParamKind::Column => {
                let input = ArrayInput::Column(columns[column]);
                column += 1;
                input
            }
            ArrayParamKind::ScalarBroadcast => {
                let input = ArrayInput::Scalar(broadcasts[broadcast]);
                broadcast += 1;
                input
            }
        })
        .collect::<Vec<_>>();
    let features = requested.supported_by_host();
    let plan = ArrayPlan::for_len_with_features(elements, features);
    evaluate_lowered_array_with_features(&array_function.element, &inputs, plan, features)
}

/// Reduces the per-row results of a pure all-f64 expression in source order.
/// Packed chunks are used for the expression evaluation when available, then
/// their lanes are accumulated left-to-right so the reduction order is
/// deterministic. Loops, libm calls, and unsupported operations use the
/// scalar interpreter for the complete reduction.
pub fn reduce_sum(source: &str, columns: &[&[f64]]) -> Result<ReductionResult, String> {
    reduce_sum_with_features(source, columns, CpuFeatures::detect())
}

/// Reduces an array expression using the requested, host-safe CPU feature
/// mask. The reduction order remains the same source order as [`reduce_sum`].
pub fn reduce_sum_with_features(
    source: &str,
    columns: &[&[f64]],
    requested: CpuFeatures,
) -> Result<ReductionResult, String> {
    let features = requested.supported_by_host();
    let (function, plan) = prepare_array(source, columns, features)?;
    let inputs = columns
        .iter()
        .copied()
        .map(ArrayInput::Column)
        .collect::<Vec<_>>();
    if plan.width != SimdWidth::Scalar {
        let lanes = plan.width.lanes();
        if let Some(chunks) = try_evaluate_packed_loop(&function, &inputs, plan, features) {
            let mut used_packed = plan.full_chunks > 0;
            let mut values = chunks;
            if plan.tail > 0 {
                if let Some(tail) = try_evaluate_packed_tail(
                    &function,
                    &inputs,
                    plan.full_chunks * lanes,
                    plan.width,
                    plan.tail,
                    features,
                ) {
                    values.extend(tail);
                    used_packed = true;
                } else {
                    for index in plan.full_chunks * lanes..plan.elements {
                        let args = columns
                            .iter()
                            .map(|column| column[index])
                            .collect::<Vec<_>>();
                        values.push(evaluate_scalar(source, &args)?);
                    }
                }
            }
            if used_packed && values.len() == plan.elements {
                let value = values.into_iter().sum::<f64>();
                return Ok(ReductionResult {
                    value,
                    plan,
                    used_packed_backend: true,
                });
            }
        }
    }

    let mut value = 0.0;
    for index in 0..plan.elements {
        let args = columns
            .iter()
            .map(|column| column[index])
            .collect::<Vec<_>>();
        value += evaluate_scalar(source, &args)?;
    }
    Ok(ReductionResult {
        value,
        plan,
        used_packed_backend: false,
    })
}

trait PackedOps {
    type Vector: Copy;
    const LANES: usize;
    const HAS_FMA: bool = false;

    unsafe fn splat(value: f64) -> Self::Vector;
    unsafe fn load(values: *const f64) -> Self::Vector;
    unsafe fn store(value: Self::Vector, values: *mut f64);
    unsafe fn load_masked(values: *const f64, active: usize) -> Result<Self::Vector, ()> {
        if active == Self::LANES {
            Ok(Self::load(values))
        } else {
            Err(())
        }
    }
    unsafe fn store_masked(value: Self::Vector, values: *mut f64, active: usize) -> Result<(), ()> {
        if active == Self::LANES {
            Self::store(value, values);
            Ok(())
        } else {
            Err(())
        }
    }
    unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn min(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn max(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn sqrt(value: Self::Vector) -> Self::Vector;
    unsafe fn abs(value: Self::Vector) -> Self::Vector;
    unsafe fn floor(value: Self::Vector) -> Result<Self::Vector, ()> {
        let _ = value;
        Err(())
    }
    unsafe fn ceil(value: Self::Vector) -> Result<Self::Vector, ()> {
        let _ = value;
        Err(())
    }
    unsafe fn round(value: Self::Vector) -> Result<Self::Vector, ()> {
        let _ = value;
        Err(())
    }
    unsafe fn trunc(value: Self::Vector) -> Result<Self::Vector, ()> {
        let _ = value;
        Err(())
    }
    unsafe fn cmp(op: CmpOp, lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn select(mask: Self::Vector, then_: Self::Vector, else_: Self::Vector) -> Self::Vector;
    unsafe fn fma(lhs: Self::Vector, rhs: Self::Vector, addend: Self::Vector) -> Self::Vector {
        Self::add(Self::mul(lhs, rhs), addend)
    }
}

fn get_packed<V: PackedOps>(values: &[Option<V::Vector>], value: Value) -> Result<V::Vector, ()> {
    values
        .get(value.0 as usize)
        .and_then(|value| *value)
        .ok_or(())
}

unsafe fn evaluate_packed_loop<V: PackedOps>(
    function: &Function,
    inputs: &[ArrayInput<'_>],
    plan: ArrayPlan,
) -> Result<Vec<f64>, ()> {
    let vector_loop =
        lower_f64_vector_loop(function, V::LANES as u8, plan.elements).map_err(|_| ())?;
    if vector_loop.store.value != vector_loop.body.result
        || vector_loop.store.lanes as usize != V::LANES
    {
        return Err(());
    }
    let mut values = Vec::with_capacity(vector_loop.full_chunks * vector_loop.lanes as usize);
    for iteration in 0..vector_loop.full_chunks {
        let start = vector_loop
            .induction_start
            .checked_add(iteration * vector_loop.induction_step)
            .ok_or(())?;
        values.extend(evaluate_vector_function::<V>(
            function,
            &vector_loop.body,
            inputs,
            start,
            vector_loop.lanes as usize,
        )?);
    }
    Ok(values)
}

#[cfg(target_arch = "x86_64")]
unsafe fn evaluate_packed_with_active<V: PackedOps>(
    function: &Function,
    inputs: &[ArrayInput<'_>],
    start: usize,
    active: usize,
) -> Result<Vec<f64>, ()> {
    if active == 0 || active > V::LANES {
        return Err(());
    }
    let vector = lower_f64_vector(function, V::LANES as u8).map_err(|_| ())?;
    evaluate_vector_function::<V>(function, &vector, inputs, start, active)
}

unsafe fn evaluate_vector_function<V: PackedOps>(
    function: &Function,
    vector: &VectorFunction,
    inputs: &[ArrayInput<'_>],
    start: usize,
    active: usize,
) -> Result<Vec<f64>, ()> {
    let value_count = vector
        .insts
        .iter()
        .map(|(value, _)| value.0 as usize + 1)
        .max()
        .unwrap_or(0);
    let mut values = vec![None; value_count.max(function.insts.len())];
    for &(value, inst) in &vector.insts {
        let result = match inst {
            VectorInst::SplatF64(bits) => V::splat(f64::from_bits(bits)),
            VectorInst::Param { index } => match inputs.get(index as usize).ok_or(())? {
                ArrayInput::Column(column) => {
                    if start.checked_add(active).ok_or(())? > column.len() {
                        return Err(());
                    }
                    V::load_masked(column[start..].as_ptr(), active)?
                }
                ArrayInput::Scalar(value) => V::splat(*value),
            },
            VectorInst::VecLoad {
                base,
                offset,
                lanes,
            } => {
                if usize::from(lanes) != V::LANES {
                    return Err(());
                }
                let Some(Inst::Param { index, ty: Ty::F64 }) = function.insts.get(base.0 as usize)
                else {
                    return Err(());
                };
                let begin = if offset >= 0 {
                    start.checked_add(offset as usize).ok_or(())?
                } else {
                    start
                        .checked_sub(offset.unsigned_abs() as usize)
                        .ok_or(())?
                };
                let ArrayInput::Column(column) = inputs.get(*index as usize).ok_or(())? else {
                    return Err(());
                };
                if begin.checked_add(active).ok_or(())? > column.len() {
                    return Err(());
                }
                V::load_masked(column[begin..].as_ptr(), active)?
            }
            VectorInst::Move(value) => get_packed::<V>(&values, value)?,
            VectorInst::Add(lhs, rhs) => V::add(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Sub(lhs, rhs) => V::sub(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Mul(lhs, rhs) => V::mul(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Div(lhs, rhs) => V::div(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Min(lhs, rhs) => V::min(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Max(lhs, rhs) => V::max(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Neg(value) => V::sub(V::splat(0.0), get_packed::<V>(&values, value)?),
            VectorInst::Sqrt(value) => V::sqrt(get_packed::<V>(&values, value)?),
            VectorInst::Abs(value) => V::abs(get_packed::<V>(&values, value)?),
            VectorInst::Floor(value) => V::floor(get_packed::<V>(&values, value)?)?,
            VectorInst::Ceil(value) => V::ceil(get_packed::<V>(&values, value)?)?,
            VectorInst::Round(value) => V::round(get_packed::<V>(&values, value)?)?,
            VectorInst::Trunc(value) => V::trunc(get_packed::<V>(&values, value)?)?,
            VectorInst::Fma { a, b, c } => {
                if !V::HAS_FMA {
                    return Err(());
                }
                V::fma(
                    get_packed::<V>(&values, a)?,
                    get_packed::<V>(&values, b)?,
                    get_packed::<V>(&values, c)?,
                )
            }
            VectorInst::Cmp { op, lhs, rhs } => V::cmp(
                op,
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Select { cond, then_, else_ } => V::select(
                get_packed::<V>(&values, cond)?,
                get_packed::<V>(&values, then_)?,
                get_packed::<V>(&values, else_)?,
            ),
            VectorInst::MaskAnd(lhs, rhs) => V::mul(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::MaskOr(lhs, rhs) => {
                let lhs = get_packed::<V>(&values, lhs)?;
                let rhs = get_packed::<V>(&values, rhs)?;
                let both = V::mul(lhs, rhs);
                V::sub(V::add(lhs, rhs), both)
            }
            VectorInst::MaskXor(lhs, rhs) => {
                let lhs = get_packed::<V>(&values, lhs)?;
                let rhs = get_packed::<V>(&values, rhs)?;
                let both = V::mul(lhs, rhs);
                V::sub(V::sub(V::add(lhs, rhs), both), both)
            }
            VectorInst::MaskNot(value) => V::sub(V::splat(1.0), get_packed::<V>(&values, value)?),
            VectorInst::VecStore { .. } | VectorInst::VecReduce { .. } => return Err(()),
        };
        values[value.0 as usize] = Some(result);
    }
    let result = get_packed::<V>(&values, vector.result)?;
    let mut output = vec![0.0; V::LANES];
    V::store_masked(result, output.as_mut_ptr(), active)?;
    output.truncate(active);
    Ok(output)
}

fn try_evaluate_packed_loop(
    function: &Function,
    inputs: &[ArrayInput<'_>],
    plan: ArrayPlan,
    features: CpuFeatures,
) -> Option<Vec<f64>> {
    #[cfg(target_arch = "x86_64")]
    if plan.width == SimdWidth::F64x8
        && features.avx512f
        && (!function_uses_fma(function) || features.fma)
        && std::is_x86_feature_detected!("avx512f")
    {
        // SAFETY: runtime feature detection proves AVX-512F is available.
        return unsafe { evaluate_packed_loop::<x86_packed::Avx512>(function, inputs, plan) }.ok();
    }
    #[cfg(target_arch = "x86_64")]
    if plan.width == SimdWidth::F64x4 && features.avx2 && std::is_x86_feature_detected!("avx2") {
        // SAFETY: runtime feature detection proves AVX2 is available. FMA is
        // selected separately because fused multiply-add changes rounding.
        if function_uses_fma(function) {
            if !features.fma || !std::is_x86_feature_detected!("fma") {
                return None;
            }
            return unsafe { evaluate_packed_loop::<x86_packed::Avx2Fma>(function, inputs, plan) }
                .ok();
        }
        return unsafe { evaluate_packed_loop::<x86_packed::Avx2>(function, inputs, plan) }.ok();
    }
    #[cfg(target_arch = "x86_64")]
    if plan.width == SimdWidth::F64x2 && features.sse2 && std::is_x86_feature_detected!("sse2") {
        if features.sse41 && std::is_x86_feature_detected!("sse4.1") {
            // SAFETY: SSE4.1 is guaranteed by the runtime check and lane
            // ranges were checked inside evaluate_packed.
            return unsafe { evaluate_packed_loop::<x86_packed::Sse41>(function, inputs, plan) }
                .ok();
        }
        // SAFETY: SSE2 is guaranteed by the runtime check and lane ranges
        // were checked inside evaluate_packed. SSE2 remains the scalar
        // fallback for operations, such as ties-away-from-zero round, that
        // do not have the required packed primitive.
        return unsafe { evaluate_packed_loop::<x86_packed::Sse2>(function, inputs, plan) }.ok();
    }
    #[cfg(target_arch = "aarch64")]
    if plan.width == SimdWidth::F64x2 && features.neon {
        // AArch64 always provides the NEON register set used here.
        return unsafe { evaluate_packed_loop::<neon_packed::Neon>(function, inputs, plan) }.ok();
    }
    None
}

fn try_evaluate_packed_tail(
    function: &Function,
    inputs: &[ArrayInput<'_>],
    start: usize,
    width: SimdWidth,
    active: usize,
    features: CpuFeatures,
) -> Option<Vec<f64>> {
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (function, inputs, start, width, active, features);

    #[cfg(target_arch = "x86_64")]
    if width == SimdWidth::F64x8 && features.avx512f && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: AVX-512F is runtime-gated and the masked load/store only
        // accesses the `active` elements that remain in each input column.
        return unsafe {
            evaluate_packed_with_active::<x86_packed::Avx512>(function, inputs, start, active)
        }
        .ok();
    }
    None
}

#[cfg(target_arch = "x86_64")]
fn function_uses_fma(function: &Function) -> bool {
    function
        .insts
        .iter()
        .any(|inst| matches!(inst, Inst::Fma { .. }))
}

#[cfg(target_arch = "x86_64")]
mod x86_packed {
    use super::{CmpOp, PackedOps};
    use std::arch::asm;
    use std::arch::x86_64::*;

    pub struct Sse2;
    pub struct Sse41;
    pub struct Avx2;

    /// AVX-512 implementation using stable inline assembly rather than the
    /// still-unstable Rust AVX-512 intrinsics. Every operation is reached only
    /// after `is_x86_feature_detected!("avx512f")` succeeds, and the compiler
    /// is not asked to generate AVX-512 on the portable code path.
    pub struct Avx512;

    #[inline]
    fn forge_min_scalar(lhs: f64, rhs: f64) -> f64 {
        if lhs.is_nan() || rhs.is_nan() {
            if lhs.is_nan() {
                rhs
            } else {
                lhs
            }
        } else if lhs < rhs {
            lhs
        } else if rhs < lhs {
            rhs
        } else if lhs == 0.0 && rhs == 0.0 {
            f64::from_bits(lhs.to_bits() | rhs.to_bits())
        } else {
            lhs
        }
    }

    #[inline]
    fn forge_max_scalar(lhs: f64, rhs: f64) -> f64 {
        if lhs.is_nan() || rhs.is_nan() {
            if lhs.is_nan() {
                rhs
            } else {
                lhs
            }
        } else if lhs > rhs {
            lhs
        } else if rhs > lhs {
            rhs
        } else if lhs == 0.0 && rhs == 0.0 {
            f64::from_bits(lhs.to_bits() & rhs.to_bits())
        } else {
            lhs
        }
    }

    macro_rules! avx512_binary {
        ($name:ident, $instruction:literal) => {
            unsafe fn $name(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                let mut output = [0.0; 8];
                asm!(
                    "vmovupd zmm0, [{lhs}]",
                    "vmovupd zmm1, [{rhs}]",
                    concat!($instruction, " zmm0, zmm0, zmm1"),
                    "vmovupd [{out}], zmm0",
                    lhs = in(reg) lhs.as_ptr(),
                    rhs = in(reg) rhs.as_ptr(),
                    out = in(reg) output.as_mut_ptr(),
                    out("zmm0") _,
                    out("zmm1") _,
                    options(nostack, preserves_flags),
                );
                output
            }
        };
    }

    macro_rules! avx512_rounding {
        ($name:ident, $mode:literal) => {
            unsafe fn $name(value: Self::Vector) -> Result<Self::Vector, ()> {
                let mut output = [0.0; 8];
                asm!(
                    "vmovupd zmm0, [{value}]",
                    concat!("vrndscalepd zmm0, zmm0, ", $mode),
                    "vmovupd [{out}], zmm0",
                    value = in(reg) value.as_ptr(),
                    out = in(reg) output.as_mut_ptr(),
                    out("zmm0") _,
                    options(nostack, preserves_flags),
                );
                Ok(output)
            }
        };
    }

    unsafe fn avx512_round(value: [f64; 8]) -> Result<[f64; 8], ()> {
        let abs_mask = [f64::from_bits(0x7fff_ffff_ffff_ffff); 8];
        let sign_mask = [f64::from_bits(0x8000_0000_0000_0000); 8];
        let half = [0.5; 8];
        let mut output = [0.0; 8];
        asm!(
            "vmovupd zmm0, [{value}]",
            "vcmppd k1, zmm0, zmm0, 3",
            "vmovupd zmm1, [{value}]",
            "vmovupd zmm2, [{abs_mask}]",
            "vandpd zmm1, zmm1, zmm2",
            "vmovupd zmm2, [{half}]",
            "vaddpd zmm1, zmm1, zmm2",
            "vrndscalepd zmm1, zmm1, 1",
            "vmovupd zmm2, [{sign_mask}]",
            "vandpd zmm0, zmm0, zmm2",
            "vorpd zmm0, zmm0, zmm1",
            "vmovupd [{out}], zmm0",
            "vmovupd zmm1, [{value}]",
            "vmovupd [{out}] {{k1}}, zmm1",
            value = in(reg) value.as_ptr(),
            abs_mask = in(reg) abs_mask.as_ptr(),
            sign_mask = in(reg) sign_mask.as_ptr(),
            half = in(reg) half.as_ptr(),
            out = in(reg) output.as_mut_ptr(),
            out("zmm0") _,
            out("zmm1") _,
            out("zmm2") _,
            out("k1") _,
            options(nostack, preserves_flags),
        );
        Ok(output)
    }

    impl PackedOps for Avx512 {
        type Vector = [f64; 8];
        const LANES: usize = 8;
        const HAS_FMA: bool = true;

        unsafe fn splat(value: f64) -> Self::Vector {
            [value; 8]
        }

        unsafe fn load(values: *const f64) -> Self::Vector {
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{src}]",
                "vmovupd [{dst}], zmm0",
                src = in(reg) values,
                dst = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                options(nostack, preserves_flags),
            );
            output
        }

        unsafe fn store(value: Self::Vector, values: *mut f64) {
            asm!(
                "vmovupd zmm0, [{src}]",
                "vmovupd [{dst}], zmm0",
                src = in(reg) value.as_ptr(),
                dst = in(reg) values,
                out("zmm0") _,
                options(nostack, preserves_flags),
            );
        }

        unsafe fn load_masked(values: *const f64, active: usize) -> Result<Self::Vector, ()> {
            if active == 0 || active > Self::LANES {
                return Err(());
            }
            let mut output = [0.0; 8];
            asm!(
                "kmovw k1, eax",
                "vmovupd zmm0 {{k1}}{{z}}, [{src}]",
                "vmovupd [{dst}], zmm0",
                src = in(reg) values,
                dst = in(reg) output.as_mut_ptr(),
                in("eax") (1u32 << active) - 1,
                out("zmm0") _,
                out("k1") _,
                options(nostack, preserves_flags),
            );
            Ok(output)
        }

        unsafe fn store_masked(
            value: Self::Vector,
            values: *mut f64,
            active: usize,
        ) -> Result<(), ()> {
            if active == 0 || active > Self::LANES {
                return Err(());
            }
            asm!(
                "kmovw k1, eax",
                "vmovupd zmm0, [{src}]",
                "vmovupd [{dst}] {{k1}}, zmm0",
                src = in(reg) value.as_ptr(),
                dst = in(reg) values,
                in("eax") (1u32 << active) - 1,
                out("zmm0") _,
                out("k1") _,
                options(nostack, preserves_flags),
            );
            Ok(())
        }

        avx512_binary!(add, "vaddpd");
        avx512_binary!(sub, "vsubpd");
        avx512_binary!(mul, "vmulpd");
        avx512_binary!(div, "vdivpd");
        avx512_rounding!(floor, "1");
        avx512_rounding!(ceil, "2");
        avx512_rounding!(trunc, "3");
        unsafe fn round(value: Self::Vector) -> Result<Self::Vector, ()> {
            avx512_round(value)
        }

        // AVX-512F is reached through stable inline assembly for arithmetic,
        // but exact Forge min/max semantics are intentionally kept in the
        // scalar lane helper: hardware min/max differs for NaNs and zeros.
        unsafe fn min(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            std::array::from_fn(|lane| forge_min_scalar(lhs[lane], rhs[lane]))
        }

        unsafe fn max(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            std::array::from_fn(|lane| forge_max_scalar(lhs[lane], rhs[lane]))
        }

        unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{value}]",
                "vsqrtpd zmm0, zmm0",
                "vmovupd [{out}], zmm0",
                value = in(reg) value.as_ptr(),
                out = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                options(nostack, preserves_flags),
            );
            output
        }

        unsafe fn abs(value: Self::Vector) -> Self::Vector {
            let mask = [f64::from_bits(0x7fff_ffff_ffff_ffff); 8];
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{value}]",
                "vmovupd zmm1, [{mask}]",
                "vandpd zmm0, zmm0, zmm1",
                "vmovupd [{out}], zmm0",
                value = in(reg) value.as_ptr(),
                mask = in(reg) mask.as_ptr(),
                out = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                out("zmm1") _,
                options(nostack, preserves_flags),
            );
            output
        }

        unsafe fn cmp(op: CmpOp, lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            std::array::from_fn(|lane| {
                let result = match op {
                    CmpOp::Eq => lhs[lane] == rhs[lane],
                    CmpOp::Ne => lhs[lane] != rhs[lane],
                    CmpOp::Lt => lhs[lane] < rhs[lane],
                    CmpOp::Le => lhs[lane] <= rhs[lane],
                    CmpOp::Gt => lhs[lane] > rhs[lane],
                    CmpOp::Ge => lhs[lane] >= rhs[lane],
                };
                if result {
                    1.0
                } else {
                    0.0
                }
            })
        }

        unsafe fn select(
            mask: Self::Vector,
            then_: Self::Vector,
            else_: Self::Vector,
        ) -> Self::Vector {
            std::array::from_fn(|lane| {
                if mask[lane] == 1.0 {
                    then_[lane]
                } else {
                    else_[lane]
                }
            })
        }

        unsafe fn fma(lhs: Self::Vector, rhs: Self::Vector, addend: Self::Vector) -> Self::Vector {
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{addend}]",
                "vmovupd zmm1, [{lhs}]",
                "vmovupd zmm2, [{rhs}]",
                "vfmadd231pd zmm0, zmm1, zmm2",
                "vmovupd [{out}], zmm0",
                addend = in(reg) addend.as_ptr(),
                lhs = in(reg) lhs.as_ptr(),
                rhs = in(reg) rhs.as_ptr(),
                out = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                out("zmm1") _,
                out("zmm2") _,
                options(nostack, preserves_flags),
            );
            output
        }
    }

    macro_rules! exact_x86_minmax {
        ($lhs:expr, $rhs:expr, $is_max:expr, $cmp:ident, $and:ident, $andnot:ident,
         $or:ident, $setzero:ident, $cast_si:ident, $cast_pd:ident, $and_si:ident,
         $andnot_si:ident, $or_si:ident, $set1_epi64x:ident) => {{
            let lhs_nan = $cmp($lhs, $lhs, _CMP_UNORD_Q);
            let rhs_nan = $cmp($rhs, $rhs, _CMP_UNORD_Q);
            let lt = $cmp($lhs, $rhs, _CMP_LT_OQ);
            let gt = $cmp($lhs, $rhs, _CMP_GT_OQ);
            let ordered = if $is_max {
                $or(
                    $and(gt, $lhs),
                    $andnot(gt, $or($and(lt, $rhs), $andnot(lt, $lhs))),
                )
            } else {
                $or(
                    $and(lt, $lhs),
                    $andnot(lt, $or($and(gt, $rhs), $andnot(gt, $lhs))),
                )
            };
            // When both inputs are NaN the interpreter returns the RHS. The
            // second selection therefore applies only to a RHS-only NaN.
            let result = $or(
                $and(lhs_nan, $rhs),
                $andnot(
                    lhs_nan,
                    $or(
                        $and($andnot(lhs_nan, rhs_nan), $lhs),
                        $andnot($andnot(lhs_nan, rhs_nan), ordered),
                    ),
                ),
            );

            let abs_mask = $set1_epi64x(0x7fff_ffff_ffff_ffffu64 as i64);
            let zero = $setzero();
            let lhs_zero = $cmp(
                $cast_pd($and_si($cast_si($lhs), abs_mask)),
                zero,
                _CMP_EQ_OQ,
            );
            let rhs_zero = $cmp(
                $cast_pd($and_si($cast_si($rhs), abs_mask)),
                zero,
                _CMP_EQ_OQ,
            );
            let both_zero = $and(lhs_zero, rhs_zero);
            let sign = if $is_max {
                $and_si($cast_si($lhs), $cast_si($rhs))
            } else {
                $or_si($cast_si($lhs), $cast_si($rhs))
            };
            let signed_zero = $cast_pd($or_si(
                $andnot_si(abs_mask, $cast_si(result)),
                $and_si(sign, abs_mask),
            ));
            $or($and(both_zero, signed_zero), $andnot(both_zero, result))
        }};
    }

    unsafe fn unavailable_128(value: __m128d) -> Result<__m128d, ()> {
        let _ = value;
        Err(())
    }

    #[target_feature(enable = "avx2")]
    unsafe fn avx2_floor(value: __m256d) -> Result<__m256d, ()> {
        Ok(_mm256_round_pd(
            value,
            _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC,
        ))
    }

    #[target_feature(enable = "avx2")]
    unsafe fn avx2_ceil(value: __m256d) -> Result<__m256d, ()> {
        Ok(_mm256_round_pd(
            value,
            _MM_FROUND_TO_POS_INF | _MM_FROUND_NO_EXC,
        ))
    }

    #[target_feature(enable = "avx2")]
    unsafe fn avx2_trunc(value: __m256d) -> Result<__m256d, ()> {
        Ok(_mm256_round_pd(
            value,
            _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC,
        ))
    }

    #[target_feature(enable = "sse4.1")]
    unsafe fn sse41_floor(value: __m128d) -> Result<__m128d, ()> {
        Ok(_mm_round_pd(
            value,
            _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC,
        ))
    }

    #[target_feature(enable = "sse4.1")]
    unsafe fn sse41_ceil(value: __m128d) -> Result<__m128d, ()> {
        Ok(_mm_round_pd(
            value,
            _MM_FROUND_TO_POS_INF | _MM_FROUND_NO_EXC,
        ))
    }

    #[target_feature(enable = "sse4.1")]
    unsafe fn sse41_trunc(value: __m128d) -> Result<__m128d, ()> {
        Ok(_mm_round_pd(value, _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC))
    }

    #[target_feature(enable = "sse4.1")]
    unsafe fn sse41_round(value: __m128d) -> Result<__m128d, ()> {
        let abs_mask = _mm_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff));
        let sign_mask = _mm_set1_pd(f64::from_bits(0x8000_0000_0000_0000));
        let nan = _mm_cmp_pd(value, value, _CMP_UNORD_Q);
        let magnitude = _mm_and_pd(value, abs_mask);
        let shifted = _mm_add_pd(magnitude, _mm_set1_pd(0.5));
        let rounded = _mm_round_pd(shifted, _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC);
        let signed = _mm_or_pd(_mm_and_pd(value, sign_mask), rounded);
        Ok(_mm_or_pd(
            _mm_and_pd(nan, value),
            _mm_andnot_pd(nan, signed),
        ))
    }

    #[target_feature(enable = "avx2")]
    unsafe fn avx2_round(value: __m256d) -> Result<__m256d, ()> {
        let abs_mask = _mm256_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff));
        let sign_mask = _mm256_set1_pd(f64::from_bits(0x8000_0000_0000_0000));
        let nan = _mm256_cmp_pd(value, value, _CMP_UNORD_Q);
        let magnitude = _mm256_and_pd(value, abs_mask);
        let shifted = _mm256_add_pd(magnitude, _mm256_set1_pd(0.5));
        let rounded = _mm256_round_pd(shifted, _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC);
        let signed = _mm256_or_pd(_mm256_and_pd(value, sign_mask), rounded);
        Ok(_mm256_or_pd(
            _mm256_and_pd(nan, value),
            _mm256_andnot_pd(nan, signed),
        ))
    }

    macro_rules! impl_x86_ops {
        ($name:ident, $vector:ty, $lanes:expr, $set1:ident, $load:ident, $store:ident,
         $add:ident, $sub:ident, $mul:ident, $div:ident, $sqrt:ident, $and:ident,
         $cmp:ident, $andnot:ident, $or:ident, $setzero:ident, $cast_si:ident,
         $cast_pd:ident, $and_si:ident, $andnot_si:ident, $or_si:ident,
         $set1_epi64x:ident, $mask:expr, $feature:literal,
         $floor:ident, $ceil:ident, $round:ident, $trunc:ident) => {
            impl PackedOps for $name {
                type Vector = $vector;
                const LANES: usize = $lanes;
                const HAS_FMA: bool = false;

                #[target_feature(enable = $feature)]
                unsafe fn splat(value: f64) -> Self::Vector {
                    $set1(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn load(values: *const f64) -> Self::Vector {
                    $load(values)
                }
                #[target_feature(enable = $feature)]
                unsafe fn store(value: Self::Vector, values: *mut f64) {
                    $store(values, value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $add(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $sub(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $mul(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $div(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn min(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    exact_x86_minmax!(
                        lhs,
                        rhs,
                        false,
                        $cmp,
                        $and,
                        $andnot,
                        $or,
                        $setzero,
                        $cast_si,
                        $cast_pd,
                        $and_si,
                        $andnot_si,
                        $or_si,
                        $set1_epi64x
                    )
                }
                #[target_feature(enable = $feature)]
                unsafe fn max(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    exact_x86_minmax!(
                        lhs,
                        rhs,
                        true,
                        $cmp,
                        $and,
                        $andnot,
                        $or,
                        $setzero,
                        $cast_si,
                        $cast_pd,
                        $and_si,
                        $andnot_si,
                        $or_si,
                        $set1_epi64x
                    )
                }
                #[target_feature(enable = $feature)]
                unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
                    $sqrt(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn abs(value: Self::Vector) -> Self::Vector {
                    $and(value, $mask)
                }
                #[target_feature(enable = $feature)]
                unsafe fn floor(value: Self::Vector) -> Result<Self::Vector, ()> {
                    $floor(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn ceil(value: Self::Vector) -> Result<Self::Vector, ()> {
                    $ceil(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn round(value: Self::Vector) -> Result<Self::Vector, ()> {
                    $round(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn trunc(value: Self::Vector) -> Result<Self::Vector, ()> {
                    $trunc(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn cmp(op: CmpOp, lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    let comparison = match op {
                        CmpOp::Eq => $cmp(lhs, rhs, _CMP_EQ_OQ),
                        CmpOp::Ne => $cmp(lhs, rhs, _CMP_NEQ_UQ),
                        CmpOp::Lt => $cmp(lhs, rhs, _CMP_LT_OQ),
                        CmpOp::Le => $cmp(lhs, rhs, _CMP_LE_OQ),
                        CmpOp::Gt => $cmp(lhs, rhs, _CMP_GT_OQ),
                        CmpOp::Ge => $cmp(lhs, rhs, _CMP_GE_OQ),
                    };
                    $and(comparison, $set1(1.0))
                }
                #[target_feature(enable = $feature)]
                unsafe fn select(
                    mask: Self::Vector,
                    then_: Self::Vector,
                    else_: Self::Vector,
                ) -> Self::Vector {
                    let mask = $cmp(mask, $set1(1.0), _CMP_EQ_OQ);
                    $or($and(mask, then_), $andnot(mask, else_))
                }
                #[target_feature(enable = $feature)]
                unsafe fn fma(
                    lhs: Self::Vector,
                    rhs: Self::Vector,
                    addend: Self::Vector,
                ) -> Self::Vector {
                    $add($mul(lhs, rhs), addend)
                }
            }
        };
    }

    impl_x86_ops!(
        Sse2,
        __m128d,
        2,
        _mm_set1_pd,
        _mm_loadu_pd,
        _mm_storeu_pd,
        _mm_add_pd,
        _mm_sub_pd,
        _mm_mul_pd,
        _mm_div_pd,
        _mm_sqrt_pd,
        _mm_and_pd,
        _mm_cmp_pd,
        _mm_andnot_pd,
        _mm_or_pd,
        _mm_setzero_pd,
        _mm_castpd_si128,
        _mm_castsi128_pd,
        _mm_and_si128,
        _mm_andnot_si128,
        _mm_or_si128,
        _mm_set1_epi64x,
        _mm_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)),
        "sse2",
        unavailable_128,
        unavailable_128,
        unavailable_128,
        unavailable_128
    );

    impl_x86_ops!(
        Sse41,
        __m128d,
        2,
        _mm_set1_pd,
        _mm_loadu_pd,
        _mm_storeu_pd,
        _mm_add_pd,
        _mm_sub_pd,
        _mm_mul_pd,
        _mm_div_pd,
        _mm_sqrt_pd,
        _mm_and_pd,
        _mm_cmp_pd,
        _mm_andnot_pd,
        _mm_or_pd,
        _mm_setzero_pd,
        _mm_castpd_si128,
        _mm_castsi128_pd,
        _mm_and_si128,
        _mm_andnot_si128,
        _mm_or_si128,
        _mm_set1_epi64x,
        _mm_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)),
        "sse4.1",
        sse41_floor,
        sse41_ceil,
        sse41_round,
        sse41_trunc
    );

    impl_x86_ops!(
        Avx2,
        __m256d,
        4,
        _mm256_set1_pd,
        _mm256_loadu_pd,
        _mm256_storeu_pd,
        _mm256_add_pd,
        _mm256_sub_pd,
        _mm256_mul_pd,
        _mm256_div_pd,
        _mm256_sqrt_pd,
        _mm256_and_pd,
        _mm256_cmp_pd,
        _mm256_andnot_pd,
        _mm256_or_pd,
        _mm256_setzero_pd,
        _mm256_castpd_si256,
        _mm256_castsi256_pd,
        _mm256_and_si256,
        _mm256_andnot_si256,
        _mm256_or_si256,
        _mm256_set1_epi64x,
        _mm256_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)),
        "avx2",
        avx2_floor,
        avx2_ceil,
        avx2_round,
        avx2_trunc
    );

    pub struct Avx2Fma;

    impl PackedOps for Avx2Fma {
        type Vector = __m256d;
        const LANES: usize = 4;
        const HAS_FMA: bool = true;

        #[target_feature(enable = "avx2")]
        unsafe fn splat(value: f64) -> Self::Vector {
            _mm256_set1_pd(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn load(values: *const f64) -> Self::Vector {
            _mm256_loadu_pd(values)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn store(value: Self::Vector, values: *mut f64) {
            _mm256_storeu_pd(values, value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_add_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_sub_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_mul_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_div_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn min(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            exact_x86_minmax!(
                lhs,
                rhs,
                false,
                _mm256_cmp_pd,
                _mm256_and_pd,
                _mm256_andnot_pd,
                _mm256_or_pd,
                _mm256_setzero_pd,
                _mm256_castpd_si256,
                _mm256_castsi256_pd,
                _mm256_and_si256,
                _mm256_andnot_si256,
                _mm256_or_si256,
                _mm256_set1_epi64x
            )
        }
        #[target_feature(enable = "avx2")]
        unsafe fn max(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            exact_x86_minmax!(
                lhs,
                rhs,
                true,
                _mm256_cmp_pd,
                _mm256_and_pd,
                _mm256_andnot_pd,
                _mm256_or_pd,
                _mm256_setzero_pd,
                _mm256_castpd_si256,
                _mm256_castsi256_pd,
                _mm256_and_si256,
                _mm256_andnot_si256,
                _mm256_or_si256,
                _mm256_set1_epi64x
            )
        }
        #[target_feature(enable = "avx2")]
        unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
            _mm256_sqrt_pd(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn abs(value: Self::Vector) -> Self::Vector {
            _mm256_and_pd(value, _mm256_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)))
        }
        #[target_feature(enable = "avx2")]
        unsafe fn floor(value: Self::Vector) -> Result<Self::Vector, ()> {
            avx2_floor(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn ceil(value: Self::Vector) -> Result<Self::Vector, ()> {
            avx2_ceil(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn round(value: Self::Vector) -> Result<Self::Vector, ()> {
            avx2_round(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn trunc(value: Self::Vector) -> Result<Self::Vector, ()> {
            avx2_trunc(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn cmp(op: CmpOp, lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            let comparison = match op {
                CmpOp::Eq => _mm256_cmp_pd(lhs, rhs, _CMP_EQ_OQ),
                CmpOp::Ne => _mm256_cmp_pd(lhs, rhs, _CMP_NEQ_UQ),
                CmpOp::Lt => _mm256_cmp_pd(lhs, rhs, _CMP_LT_OQ),
                CmpOp::Le => _mm256_cmp_pd(lhs, rhs, _CMP_LE_OQ),
                CmpOp::Gt => _mm256_cmp_pd(lhs, rhs, _CMP_GT_OQ),
                CmpOp::Ge => _mm256_cmp_pd(lhs, rhs, _CMP_GE_OQ),
            };
            _mm256_and_pd(comparison, _mm256_set1_pd(1.0))
        }
        #[target_feature(enable = "avx2")]
        unsafe fn select(
            mask: Self::Vector,
            then_: Self::Vector,
            else_: Self::Vector,
        ) -> Self::Vector {
            let mask = _mm256_cmp_pd(mask, _mm256_set1_pd(1.0), _CMP_EQ_OQ);
            _mm256_or_pd(_mm256_and_pd(mask, then_), _mm256_andnot_pd(mask, else_))
        }
        #[target_feature(enable = "avx2,fma")]
        unsafe fn fma(lhs: Self::Vector, rhs: Self::Vector, addend: Self::Vector) -> Self::Vector {
            _mm256_fmadd_pd(lhs, rhs, addend)
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon_packed {
    use super::{CmpOp, PackedOps};
    use std::arch::aarch64::*;

    #[inline]
    unsafe fn select(mask: uint64x2_t, yes: float64x2_t, no: float64x2_t) -> float64x2_t {
        vbslq_f64(mask, yes, no)
    }

    unsafe fn exact_minmax(lhs: float64x2_t, rhs: float64x2_t, is_max: bool) -> float64x2_t {
        let all_ones = vdupq_n_u64(u64::MAX);
        let lhs_nan = veorq_u64(vceqq_f64(lhs, lhs), all_ones);
        let rhs_nan = veorq_u64(vceqq_f64(rhs, rhs), all_ones);
        let lt = vcltq_f64(lhs, rhs);
        let gt = vcgtq_f64(lhs, rhs);
        let ordered = if is_max {
            select(gt, lhs, select(lt, rhs, lhs))
        } else {
            select(lt, lhs, select(gt, rhs, lhs))
        };
        let rhs_only_nan = vandq_u64(veorq_u64(lhs_nan, all_ones), rhs_nan);
        let result = select(lhs_nan, rhs, select(rhs_only_nan, lhs, ordered));

        let abs_mask = vdupq_n_u64(0x7fff_ffff_ffff_ffff);
        let both_zero = vandq_u64(
            vceqq_f64(
                vreinterpretq_f64_u64(vandq_u64(vreinterpretq_u64_f64(lhs), abs_mask)),
                vdupq_n_f64(0.0),
            ),
            vceqq_f64(
                vreinterpretq_f64_u64(vandq_u64(vreinterpretq_u64_f64(rhs), abs_mask)),
                vdupq_n_f64(0.0),
            ),
        );
        let lhs_bits = vreinterpretq_u64_f64(lhs);
        let rhs_bits = vreinterpretq_u64_f64(rhs);
        let sign = if is_max {
            vandq_u64(lhs_bits, rhs_bits)
        } else {
            vorrq_u64(lhs_bits, rhs_bits)
        };
        let signed_zero = vreinterpretq_f64_u64(vorrq_u64(
            vandq_u64(veorq_u64(abs_mask, all_ones), vreinterpretq_u64_f64(result)),
            vandq_u64(sign, abs_mask),
        ));
        select(both_zero, signed_zero, result)
    }

    pub struct Neon;

    impl PackedOps for Neon {
        type Vector = float64x2_t;
        const LANES: usize = 2;

        unsafe fn splat(value: f64) -> Self::Vector {
            vdupq_n_f64(value)
        }
        unsafe fn load(values: *const f64) -> Self::Vector {
            vld1q_f64(values)
        }
        unsafe fn store(value: Self::Vector, values: *mut f64) {
            vst1q_f64(values, value)
        }
        unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vaddq_f64(lhs, rhs)
        }
        unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vsubq_f64(lhs, rhs)
        }
        unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vmulq_f64(lhs, rhs)
        }
        unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vdivq_f64(lhs, rhs)
        }
        unsafe fn min(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            exact_minmax(lhs, rhs, false)
        }
        unsafe fn max(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            exact_minmax(lhs, rhs, true)
        }
        unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
            vsqrtq_f64(value)
        }
        unsafe fn abs(value: Self::Vector) -> Self::Vector {
            vabsq_f64(value)
        }
        unsafe fn floor(value: Self::Vector) -> Result<Self::Vector, ()> {
            Ok(vrndmq_f64(value))
        }
        unsafe fn ceil(value: Self::Vector) -> Result<Self::Vector, ()> {
            Ok(vrndpq_f64(value))
        }
        unsafe fn round(value: Self::Vector) -> Result<Self::Vector, ()> {
            Ok(vrndaq_f64(value))
        }
        unsafe fn trunc(value: Self::Vector) -> Result<Self::Vector, ()> {
            Ok(vrndq_f64(value))
        }
        unsafe fn cmp(op: CmpOp, lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            let mask = match op {
                CmpOp::Eq => vceqq_f64(lhs, rhs),
                CmpOp::Ne => veorq_u64(vceqq_f64(lhs, rhs), vdupq_n_u64(u64::MAX)),
                CmpOp::Lt => vcltq_f64(lhs, rhs),
                CmpOp::Le => vcleq_f64(lhs, rhs),
                CmpOp::Gt => vcgtq_f64(lhs, rhs),
                CmpOp::Ge => vcgeq_f64(lhs, rhs),
            };
            vreinterpretq_f64_u64(vandq_u64(mask, vreinterpretq_u64_f64(vdupq_n_f64(1.0))))
        }
        unsafe fn select(
            mask: Self::Vector,
            then_: Self::Vector,
            else_: Self::Vector,
        ) -> Self::Vector {
            vbslq_f64(vceqq_f64(mask, vdupq_n_f64(1.0)), then_, else_)
        }
    }
}

/// Selects the widest implementation supported by the current host for the
/// scalar f64 vector pipeline. AVX-512 selection is paired with runtime-gated
/// inline assembly, so this function never claims an ISA on targets where it
/// cannot be queried.
pub fn best_width() -> SimdWidth {
    match CpuFeatures::detect().best_width(Ty::F64) {
        8 => SimdWidth::F64x8,
        4 => SimdWidth::F64x4,
        2 => SimdWidth::F64x2,
        _ => SimdWidth::Scalar,
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detect_impl() -> CpuFeatures {
    CpuFeatures {
        sse2: std::is_x86_feature_detected!("sse2"),
        sse41: std::is_x86_feature_detected!("sse4.1"),
        avx: std::is_x86_feature_detected!("avx"),
        avx2: std::is_x86_feature_detected!("avx2"),
        fma: std::is_x86_feature_detected!("fma"),
        avx512f: std::is_x86_feature_detected!("avx512f"),
        avx512dq: std::is_x86_feature_detected!("avx512dq"),
        bmi2: std::is_x86_feature_detected!("bmi2"),
        neon: false,
        sve: false,
    }
}

#[cfg(target_arch = "aarch64")]
fn detect_impl() -> CpuFeatures {
    CpuFeatures {
        neon: true,
        ..CpuFeatures::scalar()
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn detect_impl() -> CpuFeatures {
    CpuFeatures::scalar()
}

pub fn host_supports_simd() -> bool {
    best_width() != SimdWidth::Scalar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_features_choose_the_widest_supported_width() {
        let mut features = CpuFeatures::scalar();
        assert_eq!(features.best_width(Ty::F64), 1);
        features.sse2 = true;
        assert_eq!(features.best_width(Ty::F64), 2);
        features.avx2 = true;
        assert_eq!(features.best_width(Ty::F64), 4);
        features.avx512f = true;
        assert_eq!(features.best_width(Ty::F64), 8);
        assert_eq!(features.best_width(Ty::I64), 1);
    }

    #[test]
    fn scalar_feature_mask_forces_exact_interpreter_fallback() {
        let input = [-3.0, -0.0, 1.5, f64::INFINITY, f64::NAN];
        let result = evaluate_array_with_features(
            "if x < 0.0 then x * x + 1.0 else x - 1.0",
            &[&input],
            CpuFeatures::scalar(),
        )
        .unwrap();
        let expected = input
            .iter()
            .map(|&x| if x < 0.0 { x * x + 1.0 } else { x - 1.0 })
            .collect::<Vec<_>>();
        assert_eq!(result.plan.width, SimdWidth::Scalar);
        assert!(!result.used_packed_backend);
        assert_eq!(
            result
                .values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn feature_mask_is_intersected_with_host_capabilities() {
        let requested = CpuFeatures {
            sse2: true,
            sse41: true,
            avx: true,
            avx2: true,
            fma: true,
            avx512f: true,
            avx512dq: true,
            bmi2: true,
            neon: true,
            sve: true,
        };
        let host = CpuFeatures::detect();
        assert_eq!(requested.supported_by_host(), host);
        assert_eq!(
            CpuFeatures::scalar().supported_by_host(),
            CpuFeatures::scalar()
        );
    }

    #[test]
    fn masked_reduction_matches_the_unmasked_result() {
        let input = (0..11).map(|value| value as f64 - 4.0).collect::<Vec<_>>();
        let masked = reduce_sum_with_features(
            "if x < 0.0 then x * x else x + 0.5",
            &[&input],
            CpuFeatures::scalar(),
        )
        .unwrap();
        let expected = input
            .iter()
            .fold(0.0, |sum, &x| sum + if x < 0.0 { x * x } else { x + 0.5 });
        assert_eq!(masked.value.to_bits(), expected.to_bits());
        assert_eq!(masked.plan.width, SimdWidth::Scalar);
        assert!(!masked.used_packed_backend);
    }

    #[test]
    fn disabling_fma_prevents_fma_packed_execution() {
        let mut features = CpuFeatures::detect();
        features.fma = false;
        let input = [1.25, -2.0, 3.5, 4.0];
        let result = evaluate_array_with_features("fma(x, x, 1.0)", &[&input], features).unwrap();
        let expected = input
            .iter()
            .map(|&value| value.mul_add(value, 1.0))
            .collect::<Vec<_>>();
        assert_eq!(
            result
                .values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
        assert!(!result.used_packed_backend);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx512_masked_tail_matches_scalar_elements_when_available() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let left = (0..17).map(|value| value as f64 - 8.0).collect::<Vec<_>>();
        let right = (0..17)
            .map(|value| value as f64 * 0.25 + 1.0)
            .collect::<Vec<_>>();
        let addend = (0..17).map(|value| value as f64 - 2.0).collect::<Vec<_>>();
        let result = evaluate_array("fma(x, y, z) + abs(x)", &[&left, &right, &addend])
            .expect("AVX-512 array evaluation");

        let expected = left
            .iter()
            .zip(&right)
            .zip(&addend)
            .map(|((x, y), z)| x.mul_add(*y, *z) + x.abs())
            .collect::<Vec<_>>();
        assert_eq!(result.plan.width, SimdWidth::F64x8);
        assert_eq!(result.values, expected);
        assert!(result.used_packed_backend);
    }

    #[test]
    fn detected_features_are_consistent_with_the_public_width() {
        let features = CpuFeatures::detect();
        assert_eq!(
            best_width().lanes(),
            usize::from(features.best_width(Ty::F64))
        );
    }

    #[test]
    fn array_fallback_handles_every_tail_length() {
        for length in 1..=100 {
            let input = (0..length).map(|value| value as f64).collect::<Vec<_>>();
            let result = evaluate_array("x * x + 1.0", &[&input]).unwrap();
            let expected = input
                .iter()
                .map(|value| value * value + 1.0)
                .collect::<Vec<_>>();
            assert_eq!(result.values, expected);
            let avx512_masked_tail = cfg!(target_arch = "x86_64")
                && result.plan.width == SimdWidth::F64x8
                && CpuFeatures::detect().avx512f
                && result.plan.tail > 0;
            assert_eq!(
                result.used_packed_backend,
                result.plan.width != SimdWidth::Scalar
                    && (result.plan.full_chunks > 0 || avx512_masked_tail)
            );
            assert_eq!(result.plan.elements, length);
            assert_eq!(
                result.plan.full_chunks * result.plan.width.lanes() + result.plan.tail,
                length
            );
        }
    }

    #[test]
    fn array_fallback_rejects_mismatched_columns() {
        let left = [1.0, 2.0];
        let right = [3.0];
        assert!(evaluate_array("x + y", &[&left, &right]).is_err());
    }

    #[test]
    fn structured_control_flow_uses_predicated_packed_execution() {
        let input = [0.0, 1.0, 2.0, 3.0];
        let result = evaluate_array("if x < 2.0 then x + 1.0 else x - 1.0", &[&input]).unwrap();
        assert_eq!(result.values, vec![1.0, 2.0, 1.0, 2.0]);
        assert_eq!(
            result.used_packed_backend,
            result.plan.width != SimdWidth::Scalar
        );
    }

    #[test]
    fn nested_control_flow_and_nan_conditions_match_the_scalar_oracle() {
        let input = [
            -3.0,
            -1.0,
            0.0,
            2.0,
            f64::NAN,
            f64::from_bits(0x8000_0000_0000_0000),
        ];
        let result = evaluate_array(
            "if x < 0.0 then (if x < -2.0 then x * x else x + 1.0) else x - 1.0",
            &[&input],
        )
        .unwrap();
        let expected = input
            .iter()
            .map(|&x| {
                if x < 0.0 {
                    if x < -2.0 {
                        x * x
                    } else {
                        x + 1.0
                    }
                } else {
                    x - 1.0
                }
            })
            .collect::<Vec<_>>();
        for (actual, expected) in result.values.iter().zip(expected) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        assert_eq!(
            result.used_packed_backend,
            result.plan.width != SimdWidth::Scalar
        );

        let inactive_nan =
            evaluate_array("if x < 0.0 then sqrt(-1.0) else 2.0", &[&[1.0, -1.0]]).unwrap();
        assert_eq!(inactive_nan.values[0].to_bits(), 2.0f64.to_bits());
        assert!(inactive_nan.values[1].is_nan());
    }

    #[test]
    fn sum_reduction_preserves_source_order_with_packed_chunks_and_tail() {
        let input = (0..11).map(|value| value as f64).collect::<Vec<_>>();
        let result = reduce_sum("x * x + 1.0", &[&input]).unwrap();
        let expected = input
            .iter()
            .fold(0.0, |sum, value| sum + value * value + 1.0);
        assert_eq!(result.value.to_bits(), expected.to_bits());
        assert_eq!(result.plan.elements, input.len());
        assert!(result.used_packed_backend == (result.plan.full_chunks > 0));
    }

    #[test]
    fn sum_reduction_uses_predicated_packed_control_flow() {
        let input = [0.0, 1.0, 2.0, 3.0];
        let result = reduce_sum("if x < 2.0 then x + 1.0 else x - 1.0", &[&input]).unwrap();
        assert_eq!(result.value, 6.0);
        assert_eq!(
            result.used_packed_backend,
            result.plan.width != SimdWidth::Scalar
        );
    }

    #[test]
    fn fma_uses_the_fused_packed_path_only_when_available() {
        let left = vec![1.0e16; 9];
        let right = vec![1.0000000000000002; 9];
        let addend = vec![-1.0e16; 9];
        let result = evaluate_array("fma(x, y, z)", &[&left, &right, &addend]).unwrap();
        let expected = left
            .iter()
            .zip(&right)
            .zip(&addend)
            .map(|((left, right), addend)| left.mul_add(*right, *addend))
            .collect::<Vec<_>>();
        assert_eq!(result.values, expected);
        let avx512_fma = cfg!(target_arch = "x86_64")
            && result.plan.width == SimdWidth::F64x8
            && CpuFeatures::detect().avx512f;
        assert_eq!(
            result.used_packed_backend,
            (result.plan.width == SimdWidth::F64x4
                && CpuFeatures::detect().avx2
                && CpuFeatures::detect().fma)
                || avx512_fma
        );
    }

    #[test]
    fn lower_f64_vector_builds_typed_lane_ir_for_the_packed_backend() {
        let function = forge_runtime::lower_source("fma(x, 2.0, y) + sqrt(abs(x))").unwrap();
        let vector = lower_f64_vector(&function, 4).unwrap();
        assert_eq!(vector.lanes, 4);
        assert!(vector.insts.iter().any(|(_, inst)| matches!(
            inst,
            VectorInst::VecLoad {
                base: Value(0),
                lanes: 4,
                ..
            }
        )));
        assert!(vector
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, VectorInst::Fma { .. })));
        assert!(vector
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, VectorInst::Sqrt(_))));
    }

    #[test]
    fn packed_rounding_operations_match_scalar_bits() {
        let input = [
            -3.75,
            -2.5,
            -0.5,
            -0.0,
            0.0,
            0.5,
            2.5,
            3.75,
            f64::INFINITY,
            f64::NAN,
        ];
        for (source, expected) in [
            ("floor(x)", input.map(f64::floor)),
            ("ceil(x)", input.map(f64::ceil)),
            ("trunc(x)", input.map(f64::trunc)),
            ("round(x)", input.map(f64::round)),
        ] {
            let result = evaluate_array(source, &[&input]).unwrap();
            let actual_bits = result
                .values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>();
            let expected_bits = expected
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>();
            assert_eq!(actual_bits, expected_bits, "{source}");

            let has_full_chunk =
                result.plan.width != SimdWidth::Scalar && result.plan.full_chunks > 0;
            let expected_packed = has_full_chunk
                && (cfg!(target_arch = "aarch64")
                    || (cfg!(target_arch = "x86_64")
                        && (result.plan.width != SimdWidth::F64x2 || CpuFeatures::detect().sse41)));
            assert_eq!(result.used_packed_backend, expected_packed, "{source}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sse41_round_preserves_nan_payloads_and_matches_scalar_bits() {
        let mut features = CpuFeatures::detect();
        if !features.sse41 {
            return;
        }
        features.avx = false;
        features.avx2 = false;
        features.fma = false;
        features.avx512f = false;
        features.avx512dq = false;
        let input = [
            -2.5,
            2.5,
            f64::from_bits(0xfff8_0000_0000_0042),
            f64::from_bits(0x7ff8_0000_0000_0043),
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        let result = evaluate_array_with_features("round(x)", &[&input], features).unwrap();
        let expected = input.map(f64::round);
        assert_eq!(result.plan.width, SimdWidth::F64x2);
        assert!(result.used_packed_backend);
        assert_eq!(
            result
                .values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn vector_loop_lowering_models_induction_and_output_store() {
        let function = forge_runtime::lower_source("x * x + 1.0").unwrap();
        let loop_ir = lower_f64_vector_loop(&function, 4, 10).unwrap();

        assert_eq!(loop_ir.induction_start, 0);
        assert_eq!(loop_ir.induction_step, 4);
        assert_eq!(loop_ir.full_chunks, 2);
        assert_eq!(loop_ir.tail, 2);
        assert_eq!(loop_ir.store.value, loop_ir.body.result);
        assert_eq!(loop_ir.store.offset, 0);
        assert_eq!(loop_ir.store.lanes, 4);
        assert_eq!(
            loop_ir
                .body
                .insts
                .iter()
                .filter(|(_, inst)| matches!(inst, VectorInst::VecLoad { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn source_vectorize_matches_scalar_for_every_tail_length() {
        let left = (0..100)
            .map(|index| index as f64 - 37.5)
            .collect::<Vec<_>>();
        let right = (0..100)
            .map(|index| (index as f64 * 0.25) - 8.0)
            .collect::<Vec<_>>();
        let addend = (0..100)
            .map(|index| if index % 3 == 0 { -0.0 } else { 1.25 })
            .collect::<Vec<_>>();
        for length in 1..=100 {
            let vectorized = evaluate_vectorized(
                "@vectorize result[i] = a[i] * b[i] + c[i]",
                &[&left[..length], &right[..length], &addend[..length]],
            )
            .unwrap();
            let scalar = evaluate_array_with_features(
                "a * b + c",
                &[&left[..length], &right[..length], &addend[..length]],
                CpuFeatures::scalar(),
            )
            .unwrap();
            assert_eq!(
                vectorized
                    .values
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                scalar
                    .values
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                "mismatch for length {length}"
            );
            assert_eq!(vectorized.plan.elements, length);
        }
    }

    #[test]
    fn source_vectorize_supports_structured_if_and_scalar_fallback() {
        let values = [-2.0, -0.0, 1.0, f64::NAN, 4.0];
        let result = evaluate_vectorized_with_features(
            "@vectorize result[i] = if a[i] < 0.0 then a[i] * a[i] else a[i] + 1.0",
            &[&values],
            CpuFeatures::scalar(),
        )
        .unwrap();
        let expected = values
            .iter()
            .map(|value| {
                if *value < 0.0 {
                    value * value
                } else {
                    value + 1.0
                }
            })
            .collect::<Vec<_>>();
        assert!(!result.used_packed_backend);
        assert_eq!(
            result
                .values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn source_vectorize_broadcasts_scalars_without_repeated_columns() {
        let values = (0..100)
            .map(|index| index as f64 - 37.5)
            .collect::<Vec<_>>();
        for length in 1..=100 {
            let vectorized = evaluate_vectorized_with_broadcasts(
                "@vectorize result[i] = a[i] * scale + bias",
                &[&values[..length]],
                &[1.25, -0.5],
            )
            .unwrap();
            let scalar = evaluate_vectorized_with_broadcasts_and_features(
                "@vectorize result[i] = a[i] * scale + bias",
                &[&values[..length]],
                &[1.25, -0.5],
                CpuFeatures::scalar(),
            )
            .unwrap();
            assert_eq!(
                vectorized
                    .values
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                scalar
                    .values
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                "broadcast mismatch for length {length}"
            );
            assert_eq!(vectorized.plan.elements, length);
        }
    }

    #[test]
    fn source_vectorize_requires_the_declared_broadcast_values() {
        let error =
            evaluate_vectorized("@vectorize result[i] = a[i] + scale", &[&[1.0, 2.0]]).unwrap_err();
        assert!(error.contains("columns and broadcasts"), "{error}");
    }

    #[test]
    fn source_vectorize_rejects_noncanonical_indexing() {
        let error =
            evaluate_vectorized("@vectorize result[i] = a[i + 1]", &[&[1.0, 2.0]]).unwrap_err();
        assert!(error.contains("declared induction variable"), "{error}");
    }

    #[test]
    fn packed_min_max_match_forge_bits_for_ieee_edges_and_all_tail_lengths() {
        let special = [
            1.0,
            -2.0,
            f64::NAN,
            f64::from_bits(0x7ff8_0000_0000_0001),
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -0.0,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
        ];
        let left = (0..100)
            .map(|index| special[index % special.len()])
            .collect::<Vec<_>>();
        let right = (0..100)
            .map(|index| special[(index * 3 + 1) % special.len()])
            .collect::<Vec<_>>();

        let forge_min = |lhs: f64, rhs: f64| {
            if lhs.is_nan() || rhs.is_nan() {
                if lhs.is_nan() {
                    rhs
                } else {
                    lhs
                }
            } else if lhs < rhs {
                lhs
            } else if rhs < lhs {
                rhs
            } else if lhs == 0.0 && rhs == 0.0 {
                f64::from_bits(lhs.to_bits() | rhs.to_bits())
            } else {
                lhs
            }
        };
        let forge_max = |lhs: f64, rhs: f64| {
            if lhs.is_nan() || rhs.is_nan() {
                if lhs.is_nan() {
                    rhs
                } else {
                    lhs
                }
            } else if lhs > rhs {
                lhs
            } else if rhs > lhs {
                rhs
            } else if lhs == 0.0 && rhs == 0.0 {
                f64::from_bits(lhs.to_bits() & rhs.to_bits())
            } else {
                lhs
            }
        };

        for length in 1..=100 {
            let min_result = evaluate_array("min(x, y)", &[&left[..length], &right[..length]])
                .expect("packed min evaluation");
            let max_result = evaluate_array("max(x, y)", &[&left[..length], &right[..length]])
                .expect("packed max evaluation");
            for index in 0..length {
                assert_eq!(
                    min_result.values[index].to_bits(),
                    forge_min(left[index], right[index]).to_bits(),
                    "min mismatch at length {length}, index {index}"
                );
                assert_eq!(
                    max_result.values[index].to_bits(),
                    forge_max(left[index], right[index]).to_bits(),
                    "max mismatch at length {length}, index {index}"
                );
            }
        }
    }

    #[test]
    fn vector_lowering_rejects_unsupported_widths_but_accepts_pure_control_flow() {
        let straight_line = forge_runtime::lower_source("x + 1.0").unwrap();
        assert!(lower_f64_vector(&straight_line, 3).is_err());

        let control_flow = forge_runtime::lower_source("if x < 0.0 then x else -x").unwrap();
        let vector = lower_f64_vector(&control_flow, 2).unwrap();
        assert!(vector
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, VectorInst::Select { .. })));
        assert!(vector.insts.iter().any(|(_, inst)| matches!(
            inst,
            VectorInst::MaskAnd(_, _) | VectorInst::MaskNot(_) | VectorInst::MaskOr(_, _)
        )));
    }
}
