use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};
use std::collections::HashMap;

/// An index into a ConstantPool's entries. Opaque outside this module,
/// same "not usable across a different pool" caveat as Label is for a
/// different Assembler (Phase 6a's precedent).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PoolIndex(usize);

impl PoolIndex {
    /// The pool-entry position this index refers to (0-based, insertion order).
    pub fn index(self) -> usize {
        self.0
    }
}

/// Deduplicating storage for the 8-byte constants (f64 literals, sign
/// masks) instruction selection needs a real memory location for.
/// Actual byte layout / RIP-relative addressing happens later, once a
/// real Assembler and PhysReg assignments exist (post-Phase-8) -- this
/// is purely the "what constants exist, and which ones are the same
/// value" bookkeeping.
///
/// `intern` dedupes on the raw u64 bit pattern alone, deliberately not
/// distinguishing "this came from an f64 literal" from "this is an
/// integer bitmask" -- e.g. i64::MIN's bits equal -0.0f64's bits, and a
/// function using both will correctly share one pool entry between them.
/// This is safe: the pool is a passive, read-only byte store, and the
/// LOAD STRATEGY is determined entirely by which MachineInst variant
/// references a PoolIndex (LoadImmF64 always means "load as f64 value";
/// FloatAbs/FloatNeg's mask_pool always means "load as bitmask"), never
/// by inspecting the pool entry itself. Keying on raw bits rather than
/// f64 equality is also what correctly keeps +0.0 (0x0) and -0.0
/// (0x8000...0) in separate entries despite comparing equal under `==`.
#[derive(Default)]
pub struct ConstantPool {
    entries: Vec<u64>,
    index_of: HashMap<u64, PoolIndex>,
}

impl ConstantPool {
    /// Returns the existing PoolIndex if `bits` was already interned,
    /// otherwise appends a new entry and returns its index. This is the
    /// ONLY way constants enter the pool -- callers never construct a
    /// PoolIndex directly (outside this module/its test submodule).
    pub fn intern(&mut self, bits: u64) -> PoolIndex {
        if let Some(&idx) = self.index_of.get(&bits) {
            return idx;
        }
        let idx = PoolIndex(self.entries.len());
        self.entries.push(bits);
        self.index_of.insert(bits, idx);
        idx
    }

    pub fn entries(&self) -> &[u64] {
        &self.entries
    }
}

/// A machine-level instruction operating on virtual registers (SSA Values,
/// reused directly from forge-ir -- one virtual register per SSA value, no
/// separate VReg type). Sits between forge-ir's `Inst` and forge-x64's
/// `Assembler` calls. Still in 3-address SSA form: `dst` is always a fresh
/// value distinct from its operands, even for opcodes that are 2-address-
/// destructive on real x86 (IntAdd/FloatAdd/And/etc). Two-address fixup
/// (Phase 7b) doesn't rewrite this form -- it only attaches coalescing
/// hints, consumed later by Phase 8's allocator and by the final
/// MachineInst-to-bytes emission step (built once Phase 8 exists), which
/// decides whether an actual copy is needed based on real register
/// assignments.
#[derive(Clone, Debug, PartialEq)]
pub enum MachineInst {
    // Constants (ConstBool lowers through LoadImmI64 as 0/1)
    LoadImmI64 {
        dst: Value,
        imm: i64,
    },
    LoadImmF64 {
        dst: Value,
        pool_index: PoolIndex,
    },

    // Integer arithmetic -- destructive (dst must end up == lhs's location)
    IntAdd {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    IntSub {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    IntMul {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    IntDiv {
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // cqo + idiv; RAX/RDX-fixed, Phase 8's concern
    IntRem {
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // same shape, takes RDX instead of RAX
    IntNeg {
        dst: Value,
        src: Value,
    },
    And {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Or {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Xor {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Not {
        dst: Value,
        src: Value,
    },
    Shl {
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // rhs must end up in CL, Phase 8's concern
    Shr {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Sar {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },

    Lea {
        dst: Value,
        base: Value,
        index: Value,
        scale: u8,
        disp: i32,
    },

    /// Branchless select: dst = (cond != 0) ? then_val : else_val. Produced
    /// ONLY by diamond fusion (find_fusable_diamonds) -- select_inst never
    /// constructs this directly, since there is no Inst::Select to match
    /// on (see the design doc). 2-address destructive like every other
    /// x86 binary op: dst starts as a copy of then_val (a coalescing hint
    /// records dst -> then_val -- see compute_coalescing_hints), and
    /// emission (deferred, task #68) overwrites it with else_val
    /// conditionally via `test cond, cond` (reads cond's already-
    /// materialized 0/1 value directly, NOT the flags from whatever
    /// produced it) followed by `cmovz dst, else_val`. Requires cond to
    /// be genuinely zero-extended to 64 bits by whatever materialized it
    /// (setcc alone only writes 1 byte) -- a real precondition on task
    /// #68's emission of IntCmp/FloatCmp, not something this variant or
    /// its selection-time construction can itself guarantee.
    IntCmov {
        dst: Value,
        cond: Value,
        then_val: Value,
        else_val: Value,
    },

    // Float arithmetic -- destructive (dst must end up == lhs's location)
    FloatAdd {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatSub {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatMul {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatDiv {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatSqrt {
        dst: Value,
        src: Value,
    },
    FloatMin {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatMax {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatRound {
        dst: Value,
        src: Value,
        mode: crate::RoundMode,
    },

    // Abs/Neg on floats: mask_pool references the sign-mask constant in
    // the selector's ConstantPool -- see machine_inst.rs's Fma/Abs/Neg
    // lowering for why this field exists (it lets the post-Phase-8
    // emission step synthesize the exact movq+andpd/xorpd sequence once
    // mask_pool's constant and dst's real register are known).
    FloatAbs {
        dst: Value,
        src: Value,
        mask_pool: PoolIndex,
    },
    FloatNeg {
        dst: Value,
        src: Value,
        mask_pool: PoolIndex,
    },

    // Comparisons -- resolved to a specific strategy at selection time
    IntCmp {
        op: CmpOp,
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // cmp + setcc, signed codes
    FloatCmp {
        op: CmpOp,
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // ucomisd + setcc, UNSIGNED codes

    // Conversions
    IntToFloat {
        dst: Value,
        src: Value,
    },
    FloatToInt {
        dst: Value,
        src: Value,
    }, // truncating (cvttsd2si)

    // libm calls -- see crates/forge-x64/src/libm.rs for address resolution.
    // Still fully virtual-register/SSA-form like every other MachineInst: the
    // real call SEQUENCE (spill live regs, marshal args into xmm0/xmm1, align
    // rsp, mov_reg_imm+call_reg, move the f64 result out of xmm0) is entirely
    // the future emission step's job, once Phase 8 assigns real registers --
    // this variant only records WHAT gets called, with WHICH SSA args, and
    // WHERE the result goes.
    CallLibm {
        dst: Value,
        func: forge_ir::LibFunc,
        args: smallvec::SmallVec<[Value; 2]>,
    },

    // Control flow
    Jump {
        target: Block,
    },
    Branch {
        cond: Value,
        then_: Block,
        else_: Block,
    },
    Return {
        value: Value,
    },

    // Parameters
    Param {
        dst: Value,
        index: u32,
    },
}

/// The result of instruction selection: a flat MachineInst sequence plus
/// the Ty of every virtual register the selector minted that ISN'T a real
/// IR value (i.e. every synthetic temp -- Fma's mul_tmp is currently the
/// only one; Abs/Neg's sign masks live in `pool` instead, not here).
/// Phase 8 needs this to know GPR-vs-XMM class for registers
/// `func.types` doesn't cover; real IR values look their Ty up in
/// `func.types` directly via this module's own `ty_of` helper.
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    /// dst -> the Value dst should end up sharing a physical register/slot
    /// with, if Phase 8's allocator can manage it. Every entry corresponds
    /// to a 2-address-destructive x86 operation where honoring the hint
    /// lets the final MachineInst-to-bytes emission step skip an
    /// otherwise-mandatory `mov dst, lhs` copy. A hint that isn't honored
    /// is not an error -- emission falls back to inserting the copy.
    pub coalescing_hints: HashMap<Value, Value>,
    pub pool: ConstantPool,
    /// (Block, first-instruction-index-in-insts) for every block, in the
    /// same RPO order `insts` itself was built in. Lets later passes
    /// (Phase 8's liveness analysis) reconstruct block boundaries --
    /// `insts` alone has no boundary markers, and the IR-instruction-to-
    /// MachineInst count isn't 1:1 (Fma emits 2, Phi/suppressed lea
    /// operands emit 0), so only `select()`'s own walk can record this
    /// correctly. A block's end is the NEXT ENTRY'S start (by list
    /// position, not by searching for a larger value -- a block that
    /// selects to zero MachineInsts makes two consecutive entries share
    /// the same start, and only positional lookup gets that block's empty
    /// range right), or `insts.len()` for the last entry in this list.
    /// Only blocks reachable from `entry` appear here, since `select()`
    /// itself only walks `reverse_postorder(func)`.
    pub block_starts: Vec<(Block, usize)>,
}

struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
    fully_fusable_scaled_indices: std::collections::HashSet<Value>,
    pool: ConstantPool,
    diamond_fusions: HashMap<Block, DiamondFusion>,
}

impl<'a> Selector<'a> {
    /// Looks up a Value's Ty whether it's a real IR value (func.types) or
    /// a synthetic temp this selector minted (synthetic_types). Value
    /// numbering is append-only across this codebase's whole optimizer
    /// pipeline (verified: no pass compacts or renumbers `f.insts`), so
    /// `next_value` seeded from `func.insts.len()` never collides with a
    /// real Value, and this dispatch on index is safe.
    fn ty_of(&self, v: Value) -> Ty {
        if (v.0 as usize) < self.func.types.len() {
            self.func.types[v.0 as usize]
        } else {
            self.synthetic_types[&v]
        }
    }

    fn fresh(&mut self, ty: Ty) -> Value {
        let v = Value(self.next_value);
        self.next_value += 1;
        self.synthetic_types.insert(v, ty);
        v
    }

    /// Deliberately has NO wildcard (`_ =>`) arm -- same exhaustiveness
    /// rationale as forge-ir's `uses_of`/`replace_in_inst`: a new `Inst`
    /// variant must get an explicit arm here, or this fails to compile.
    /// If you hit that compile error, ADD A REAL ARM, don't silence it
    /// with `_ => {}` -- that would defeat the whole point of this match.
    fn select_inst(&mut self, dst: Value, inst: &Inst) {
        match inst {
            Inst::ConstF64(bits) => {
                let pool_index = self.pool.intern(*bits);
                self.insts.push(MachineInst::LoadImmF64 { dst, pool_index });
            }
            Inst::ConstI64(v) => self.insts.push(MachineInst::LoadImmI64 { dst, imm: *v }),
            Inst::ConstBool(v) => self.insts.push(MachineInst::LoadImmI64 {
                dst,
                imm: *v as i64,
            }),
            Inst::Param { index, .. } => self.insts.push(MachineInst::Param { dst, index: *index }),

            Inst::Add(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatAdd { dst, lhs: *a, rhs: *b }),
                Ty::I64 => match find_fusable_add(self.func, *a, *b) {
                    Some((base, index, scale)) => {
                        self.insts.push(MachineInst::Lea { dst, base, index, scale, disp: 0 })
                    }
                    None => self.insts.push(MachineInst::IntAdd { dst, lhs: *a, rhs: *b }),
                },
                Ty::Bool => unreachable!("Add never applies to Bool"),
            },
            Inst::Sub(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatSub { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntSub { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Sub never applies to Bool"),
            },
            Inst::Mul(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatMul { dst, lhs: *a, rhs: *b }),
                Ty::I64 => {
                    if !self.fully_fusable_scaled_indices.contains(&dst) {
                        self.insts.push(MachineInst::IntMul { dst, lhs: *a, rhs: *b });
                    }
                    // else: fully subsumed by lea fusion, nothing to emit --
                    // same suppression discipline as Inst::Phi.
                }
                Ty::Bool => unreachable!("Mul never applies to Bool"),
            },
            Inst::Div(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatDiv { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntDiv { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Div never applies to Bool"),
            },
            Inst::Rem(a, b) => match self.ty_of(*a) {
                Ty::I64 => self.insts.push(MachineInst::IntRem { dst, lhs: *a, rhs: *b }),
                Ty::F64 => unimplemented!(
                    "float remainder (fmod) has no native x86 instruction and isn't wired to a libm call yet"
                ),
                Ty::Bool => unreachable!("Rem never applies to Bool"),
            },
            Inst::Neg(a) => match self.ty_of(*a) {
                Ty::F64 => {
                    // i64::MIN == 0x8000_0000_0000_0000: only the sign bit
                    // set. XOR-ing this into the value FLIPS the sign bit
                    // (negation) -- contrast Abs's mask below, which CLEARS
                    // it via AND instead.
                    let mask_pool = self.pool.intern(i64::MIN as u64);
                    self.insts.push(MachineInst::FloatNeg { dst, src: *a, mask_pool });
                }
                Ty::I64 => self.insts.push(MachineInst::IntNeg { dst, src: *a }),
                Ty::Bool => unreachable!("Neg never applies to Bool"),
            },
            Inst::And(a, b) => self.insts.push(MachineInst::And { dst, lhs: *a, rhs: *b }),
            Inst::Or(a, b) => self.insts.push(MachineInst::Or { dst, lhs: *a, rhs: *b }),
            Inst::Xor(a, b) => self.insts.push(MachineInst::Xor { dst, lhs: *a, rhs: *b }),
            Inst::Not(a) => self.insts.push(MachineInst::Not { dst, src: *a }),
            Inst::Shl(a, b) => {
                if !self.fully_fusable_scaled_indices.contains(&dst) {
                    self.insts.push(MachineInst::Shl { dst, lhs: *a, rhs: *b });
                }
                // else: fully subsumed by lea fusion, nothing to emit.
            }
            Inst::Shr(a, b) => self.insts.push(MachineInst::Shr { dst, lhs: *a, rhs: *b }),
            Inst::Sar(a, b) => self.insts.push(MachineInst::Sar { dst, lhs: *a, rhs: *b }),

            Inst::Min(a, b) => self.insts.push(MachineInst::FloatMin { dst, lhs: *a, rhs: *b }),
            Inst::Max(a, b) => self.insts.push(MachineInst::FloatMax { dst, lhs: *a, rhs: *b }),
            Inst::Sqrt(a) => self.insts.push(MachineInst::FloatSqrt { dst, src: *a }),
            Inst::Floor(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Floor,
            }),
            Inst::Ceil(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Ceil,
            }),
            Inst::Round(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Nearest,
            }),
            Inst::Trunc(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Truncate,
            }),

            Inst::Cmp { op, lhs, rhs } => {
                // dst's Ty is always Bool for a comparison -- dispatching on
                // dst instead of the operand would always fall into the
                // I64|Bool arm below and silently mis-lower every float
                // comparison. Dispatch on the OPERAND's type instead.
                let operand_ty = self.ty_of(*lhs);
                match operand_ty {
                    Ty::F64 => self.insts.push(MachineInst::FloatCmp {
                        op: *op,
                        dst,
                        lhs: *lhs,
                        rhs: *rhs,
                    }),
                    Ty::I64 | Ty::Bool => self.insts.push(MachineInst::IntCmp {
                        op: *op,
                        dst,
                        lhs: *lhs,
                        rhs: *rhs,
                    }),
                }
            }
            Inst::IToF(a) => self.insts.push(MachineInst::IntToFloat { dst, src: *a }),
            Inst::FToI(a) => self.insts.push(MachineInst::FloatToInt { dst, src: *a }),

            Inst::Abs(a) => {
                // 0x7FFF_FFFF_FFFF_FFFF: every bit set EXCEPT the sign bit.
                // AND-ing this into the value CLEARS the sign bit
                // (absolute value) -- contrast Neg's mask above, which
                // FLIPS it via XOR instead.
                let mask_pool = self.pool.intern(0x7FFF_FFFF_FFFF_FFFFu64);
                self.insts.push(MachineInst::FloatAbs { dst, src: *a, mask_pool });
            }
            // NOT bit-identical to a real hardware FMA (that's a single
            // rounding; this is two -- multiply rounds once, then add
            // rounds again). Correct interim behavior until AVX/FMA3
            // lands (Phase 6's VEX/AVX subsection, not yet built) --
            // documented loudly rather than silently approximated.
            Inst::Fma { a, b, c } => {
                let mul_tmp = self.fresh(Ty::F64);
                self.insts.push(MachineInst::FloatMul { dst: mul_tmp, lhs: *a, rhs: *b });
                self.insts.push(MachineInst::FloatAdd { dst, lhs: mul_tmp, rhs: *c });
            }

            Inst::Call { func, args } => {
                self.insts.push(MachineInst::CallLibm {
                    dst,
                    func: *func,
                    args: args.clone(),
                });
            }
            Inst::Phi { .. } => {
                // Deliberately emits nothing -- see the design doc's "φ
                // handling" section. This Inst's destination Value is
                // resolved entirely by Phase 8's SSA deconstruction
                // (assign the same physical register/slot where possible;
                // insert parallel-copy moves at predecessor block ends
                // otherwise). That strategy is only safe when no CFG edge
                // into a phi's block is a critical edge -- true for
                // today's if/else-only lowering, but NOT enforced or
                // checked anywhere yet. Re-verify this once loops
                // (currently a stretch goal) can introduce back-edges.
            }
        }
    }

    fn select_term(&mut self, term: &Terminator) {
        match term {
            Terminator::Return(v) => self.insts.push(MachineInst::Return { value: *v }),
            Terminator::Jump(b) => self.insts.push(MachineInst::Jump { target: *b }),
            Terminator::Branch { cond, then_, else_ } => self.insts.push(MachineInst::Branch {
                cond: *cond,
                then_: *then_,
                else_: *else_,
            }),
        }
    }
}

/// Free function (not a Selector method) so it has a single call site
/// usable both by the whole-function suppression pre-pass (which runs
/// BEFORE any Selector exists) and by Selector's Add arm during the main
/// walk -- the suppression decision and the fusion decision MUST agree
/// about which operand (if any) is "the fused one," so there is exactly
/// one implementation of this shape check, not two that could drift out
/// of sync.
///
/// Checks whether `candidate` is a real IR value (an index into
/// func.insts -- synthetic values are never Mul/Shl-defined and always
/// return None here) defined by a "scaled index" shape: `Mul(index,
/// ConstI64(k))`/`Mul(ConstI64(k), index)` for k in {2,4,8}, OR
/// `Shl(index, ConstI64(s))` for s in {1,2,3} (equivalent to k = 2^s --
/// strength-reduction rewrites the former into the latter for realistic
/// optimized input, so both are live, real shapes on different execution
/// tiers). If matched, returns (base, index, scale) with `base` set to
/// the OTHER argument passed in.
fn match_scaled_index(
    func: &Function,
    candidate: Value,
    other: Value,
) -> Option<(Value, Value, u8)> {
    if (candidate.0 as usize) >= func.insts.len() {
        return None;
    }
    let const_scale = |v: Value| -> Option<u8> {
        if (v.0 as usize) >= func.insts.len() {
            return None;
        }
        match &func.insts[v.0 as usize] {
            Inst::ConstI64(k) if matches!(k, 2 | 4 | 8) => Some(*k as u8),
            _ => None,
        }
    };
    match &func.insts[candidate.0 as usize] {
        Inst::Mul(m_a, m_b) => {
            if let Some(k) = const_scale(*m_b) {
                return Some((other, *m_a, k));
            }
            if let Some(k) = const_scale(*m_a) {
                return Some((other, *m_b, k));
            }
            None
        }
        Inst::Shl(index, shift_amount) => {
            if (shift_amount.0 as usize) >= func.insts.len() {
                return None;
            }
            match &func.insts[shift_amount.0 as usize] {
                Inst::ConstI64(s) if matches!(s, 1..=3) => Some((other, *index, 1u8 << s)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Tries both operand orderings of an Add(a, b) for the scaled-index
/// shape, preferring `a` as the fused operand if both individually
/// qualify (Add(Mul(x,4), Shl(y,3)) picks x/4 as index, leaving y's Shl
/// as an ordinary Value feeding the Lea's `base` -- NOT itself
/// fused/suppressed).
fn find_fusable_add(func: &Function, a: Value, b: Value) -> Option<(Value, Value, u8)> {
    match_scaled_index(func, a, b).or_else(|| match_scaled_index(func, b, a))
}

/// Run once, before the main RPO walk, over the WHOLE function. Determines
/// which real IR Values, if any, are Mul/Shl results fully subsumed by lea
/// fusion (every use is a fusable Add pattern, none escape to any other
/// consumer) and therefore safe to suppress the same way Phi is.
fn find_fully_fusable_scaled_indices(func: &Function) -> std::collections::HashSet<Value> {
    let mut total_uses: HashMap<Value, u32> = HashMap::new();
    for inst in &func.insts {
        for used in forge_ir::uses_of(inst) {
            *total_uses.entry(used).or_insert(0) += 1;
        }
    }
    // uses_of only covers Inst, never Terminator -- a directly-returned or
    // branched-on Value must still count as used, or it would be wrongly
    // suppressed even though the terminator needs its real computed value.
    for block in &func.blocks {
        match &block.term {
            Some(Terminator::Return(v)) => *total_uses.entry(*v).or_insert(0) += 1,
            Some(Terminator::Branch { cond, .. }) => *total_uses.entry(*cond).or_insert(0) += 1,
            _ => {}
        }
    }

    // NOTE: this must key on the OUTER Add's own operand (`a` or `b` --
    // one of which IS the Mul/Shl's defining Value) that matched, NOT on
    // match_scaled_index's returned `index` (which is the raw scaled
    // register INSIDE the matched Mul/Shl -- a completely different
    // Value). Keying on the wrong Value here silently defeats suppression
    // entirely.
    let mut fusable_uses: HashMap<Value, u32> = HashMap::new();
    for inst in &func.insts {
        if let Inst::Add(a, b) = inst {
            if match_scaled_index(func, *a, *b).is_some() {
                *fusable_uses.entry(*a).or_insert(0) += 1;
            } else if match_scaled_index(func, *b, *a).is_some() {
                *fusable_uses.entry(*b).or_insert(0) += 1;
            }
        }
    }

    total_uses
        .into_iter()
        .filter(|(v, total)| fusable_uses.get(v).copied().unwrap_or(0) == *total)
        .map(|(v, _)| v)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MinMaxOp {
    Min,
    Max,
}

/// A diamond eligible for branchless fusion (see design doc). Keyed by
/// the diamond's BRANCH block in the map `find_fusable_diamonds` returns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DiamondFusion {
    FloatMinMax {
        dst: Value,
        op: MinMaxOp,
        lhs: Value,
        rhs: Value,
    },
    IntCmov {
        dst: Value,
        cond: Value,
        then_val: Value,
        else_val: Value,
    },
}

/// Run once, before the main RPO walk, over the WHOLE function -- exact
/// architectural mirror of `find_fully_fusable_scaled_indices` above.
/// Detects every diamond eligible for fusion: a Branch block whose two
/// arms are BOTH EMPTY (BlockData::insts.is_empty()) and both Jump to
/// the SAME merge block, which has no other predecessor, and which has
/// exactly one Phi whose two incoming values genuinely DIFFER (any other
/// Phis at the merge block must be trivial -- same value on both edges
/// -- or the diamond isn't fused; a real corpus function can have
/// several phis at one merge block, only one of which is this diamond's
/// own payload). See design doc for the full correctness reasoning
/// (why empty arms specifically, why this can't just use `RegClass`).
pub fn find_fusable_diamonds(
    func: &Function,
) -> (
    HashMap<Block, DiamondFusion>,
    std::collections::HashSet<Block>,
) {
    let mut fusions = HashMap::new();
    let mut skip = std::collections::HashSet::new();

    // Predecessor counts re-derived from real terminators, never trusted
    // from BlockData::preds (which is Builder's own bookkeeping and can
    // be stale on a hand-built Function) -- same discipline already
    // established in forge-regalloc/src/intervals.rs's critical-edge check.
    let mut pred_count: HashMap<Block, u32> = HashMap::new();
    for block in &func.blocks {
        match &block.term {
            Some(Terminator::Jump(b)) => *pred_count.entry(*b).or_insert(0) += 1,
            Some(Terminator::Branch { then_, else_, .. }) => {
                *pred_count.entry(*then_).or_insert(0) += 1;
                if then_ != else_ {
                    *pred_count.entry(*else_).or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }

    let ty_of = |v: Value| -> Ty {
        if (v.0 as usize) < func.types.len() {
            func.types[v.0 as usize]
        } else {
            // No synthetic values exist at the IR level (only select()
            // mints those) -- func.types must always cover a real IR Value.
            unreachable!("Value {v:?} has no IR-level type")
        }
    };

    for (idx, block_data) in func.blocks.iter().enumerate() {
        let pred = Block(idx as u32);
        let Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        }) = &block_data.term
        else {
            continue;
        };
        if t == e {
            continue;
        }
        let t_data = &func.blocks[t.0 as usize];
        let e_data = &func.blocks[e.0 as usize];
        if !t_data.insts.is_empty() || !e_data.insts.is_empty() {
            continue;
        }
        let (Some(Terminator::Jump(mt)), Some(Terminator::Jump(me))) = (&t_data.term, &e_data.term)
        else {
            continue;
        };
        if mt != me {
            continue;
        }
        let m = *mt;
        if pred_count.get(&m).copied().unwrap_or(0) != 2 {
            continue;
        }
        // Both arms must ALSO have exactly one predecessor each (the
        // Branch block itself) -- found by mutation testing during plan
        // review: without this check, a third, unrelated block that also
        // jumps INTO an otherwise-eligible arm block still gets skipped by
        // select() (both arms are still "empty," matching rule 2), so that
        // third block's real Jump target vanishes with nothing to redirect
        // it -- unreachable from this front-end's structured if/else
        // lowering today, but not something find_fusable_diamonds's other
        // checks happen to rule out on their own, so it's checked directly.
        if pred_count.get(t).copied().unwrap_or(0) != 1
            || pred_count.get(e).copied().unwrap_or(0) != 1
        {
            continue;
        }

        let m_data = &func.blocks[m.0 as usize];
        let mut payload: Option<(Value, Value, Value)> = None; // (phi_dst, val_t, val_e)
        let mut two_differing = false;
        for &v in &m_data.insts {
            if let Inst::Phi { incoming } = &func.insts[v.0 as usize] {
                if incoming.len() != 2 {
                    continue;
                }
                let (b0, v0) = incoming[0];
                let (b1, v1) = incoming[1];
                let (val_t, val_e) = if b0 == *t && b1 == *e {
                    (v0, v1)
                } else if b0 == *e && b1 == *t {
                    (v1, v0)
                } else {
                    continue; // this phi's edges don't match t/e at all -- ignore
                };
                if val_t != val_e {
                    if payload.is_some() {
                        two_differing = true;
                    }
                    payload = Some((v, val_t, val_e));
                }
            }
        }
        if two_differing {
            continue;
        }
        let Some((dst, val_t, val_e)) = payload else {
            continue; // no differing phi at all -- nothing this diamond fuses
        };

        // Float min/max table (see design doc for the corrected 4-row
        // derivation -- else-arm value is ALWAYS rhs, never determined by
        // the comparison operator alone).
        if let Inst::Cmp { op, lhs, rhs } = &func.insts[cond.0 as usize] {
            if ty_of(*lhs) == Ty::F64 {
                let rewrite = match op {
                    CmpOp::Lt if val_t == *lhs && val_e == *rhs => {
                        Some(DiamondFusion::FloatMinMax {
                            dst,
                            op: MinMaxOp::Min,
                            lhs: *lhs,
                            rhs: *rhs,
                        })
                    }
                    CmpOp::Lt if val_t == *rhs && val_e == *lhs => {
                        Some(DiamondFusion::FloatMinMax {
                            dst,
                            op: MinMaxOp::Max,
                            lhs: *rhs,
                            rhs: *lhs,
                        })
                    }
                    CmpOp::Gt if val_t == *lhs && val_e == *rhs => {
                        Some(DiamondFusion::FloatMinMax {
                            dst,
                            op: MinMaxOp::Max,
                            lhs: *lhs,
                            rhs: *rhs,
                        })
                    }
                    CmpOp::Gt if val_t == *rhs && val_e == *lhs => {
                        Some(DiamondFusion::FloatMinMax {
                            dst,
                            op: MinMaxOp::Min,
                            lhs: *rhs,
                            rhs: *lhs,
                        })
                    }
                    _ => None,
                };
                if let Some(fusion) = rewrite {
                    fusions.insert(pred, fusion);
                    skip.insert(*t);
                    skip.insert(*e);
                }
                // F64 comparison but not the min/max shape (Le/Ge, a
                // third unrelated value, or Eq/Ne) -- NEVER falls through
                // to IntCmov (that path is Ty::I64/Bool only). Simply
                // unfused, whether or not `rewrite` matched above.
                continue;
            }
        }

        // General integer path -- hard type gate, not a fallback. `dst`'s
        // type (not `cond`'s) determines whether IntCmov is legal, since
        // `dst` is what the cmov actually writes.
        if ty_of(dst) != Ty::F64 {
            fusions.insert(
                pred,
                DiamondFusion::IntCmov {
                    dst,
                    cond: *cond,
                    then_val: val_t,
                    else_val: val_e,
                },
            );
            skip.insert(*t);
            skip.insert(*e);
        }
    }

    (fusions, skip)
}

pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_scaled_indices = find_fully_fusable_scaled_indices(func);
    let (diamond_fusions, diamond_skip_blocks) = find_fusable_diamonds(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_scaled_indices,
        pool: ConstantPool::default(),
        diamond_fusions,
    };
    let mut block_starts = Vec::new();
    for block in forge_ir::dominance::reverse_postorder(func) {
        block_starts.push((block, sel.insts.len()));
        if diamond_skip_blocks.contains(&block) {
            // An empty diamond arm -- contributes NOTHING to insts, same
            // "two adjacent block_starts entries share one start" shape
            // as a block that selects to zero MachineInsts elsewhere in
            // this file (Phi, fully-suppressed Mul/Shl).
            continue;
        }
        for &v in &func.blocks[block.0 as usize].insts {
            let inst = &func.insts[v.0 as usize];
            sel.select_inst(v, inst);
        }
        if let Some(fusion) = sel.diamond_fusions.get(&block).copied() {
            // This block's real Branch terminator is replaced by the
            // fused instruction -- no Jump/Branch emitted for it at all.
            match fusion {
                DiamondFusion::FloatMinMax {
                    dst,
                    op: MinMaxOp::Min,
                    lhs,
                    rhs,
                } => {
                    sel.insts.push(MachineInst::FloatMin { dst, lhs, rhs });
                }
                DiamondFusion::FloatMinMax {
                    dst,
                    op: MinMaxOp::Max,
                    lhs,
                    rhs,
                } => {
                    sel.insts.push(MachineInst::FloatMax { dst, lhs, rhs });
                }
                DiamondFusion::IntCmov {
                    dst,
                    cond,
                    then_val,
                    else_val,
                } => {
                    sel.insts.push(MachineInst::IntCmov {
                        dst,
                        cond,
                        then_val,
                        else_val,
                    });
                }
            }
            // The fused instruction falls through directly into the merge
            // block `m` -- re-derived here from `t`'s own terminator (`t`
            // is always Jump(m) by find_fusable_diamonds's own eligibility
            // rules), NOT assumed. This debug_assert checks m's IDENTITY,
            // not merely "something comes next" -- a check against only
            // "is there a next block at all" would be trivially true for
            // any fused diamond (m always exists and is never itself a
            // skip block, since it has real computation: the payload phi
            // is what made this a fusion in the first place), so it must
            // name m explicitly to mean anything.
            let Terminator::Branch { then_: t, .. } =
                func.blocks[block.0 as usize].term.as_ref().unwrap()
            else {
                unreachable!("a fusion key's block must have a Branch terminator")
            };
            let Some(Terminator::Jump(m)) = &func.blocks[t.0 as usize].term else {
                unreachable!("find_fusable_diamonds guarantees t's terminator is Jump(m)")
            };
            debug_assert!(
                {
                    let rpo = forge_ir::dominance::reverse_postorder(func);
                    let this_pos = rpo.iter().position(|b| *b == block).unwrap();
                    let next_real = rpo[this_pos + 1..]
                        .iter()
                        .find(|b| !diamond_skip_blocks.contains(b));
                    // Confirmed true by execution across every real corpus
                    // program and every hand-built fixture during plan
                    // review, but this front-end's structured-CFG guarantee
                    // is not something find_fusable_diamonds itself
                    // enforces, so a violation must fail loudly here rather
                    // than silently fall through into the wrong block with
                    // no Jump to redirect it.
                    next_real == Some(m)
                },
                "diamond fusion's merge block was not the next RPO-visited block"
            );
        } else if let Some(term) = &func.blocks[block.0 as usize].term {
            sel.select_term(term);
        }
    }
    let coalescing_hints = compute_coalescing_hints(&sel.insts);
    SelectedFunction {
        insts: sel.insts,
        synthetic_types: sel.synthetic_types,
        coalescing_hints,
        pool: sel.pool,
        block_starts,
    }
}

/// Scans a fully-selected instruction sequence and records a dst->operand
/// coalescing hint for every 2-address-destructive MachineInst. Binary ops
/// hint dst->lhs (the operand whose register `dst` needs to already hold);
/// unary ops hint dst->src. IntDiv/IntRem are deliberately excluded -- their
/// constraint is fixed RAX/RDX placement, a different (fixed-register, not
/// coalescing) hint Phase 8's allocator handles separately. Lea is
/// deliberately excluded too -- real x86 lea is non-destructive 3-operand,
/// so it has no two-address constraint to hint around at all.
pub fn compute_coalescing_hints(insts: &[MachineInst]) -> HashMap<Value, Value> {
    let mut hints = HashMap::new();
    for inst in insts {
        match inst {
            MachineInst::IntAdd { dst, lhs, .. }
            | MachineInst::IntSub { dst, lhs, .. }
            | MachineInst::IntMul { dst, lhs, .. }
            | MachineInst::And { dst, lhs, .. }
            | MachineInst::Or { dst, lhs, .. }
            | MachineInst::Xor { dst, lhs, .. }
            | MachineInst::Shl { dst, lhs, .. }
            | MachineInst::Shr { dst, lhs, .. }
            | MachineInst::Sar { dst, lhs, .. }
            | MachineInst::FloatAdd { dst, lhs, .. }
            | MachineInst::FloatSub { dst, lhs, .. }
            | MachineInst::FloatMul { dst, lhs, .. }
            | MachineInst::FloatDiv { dst, lhs, .. }
            | MachineInst::FloatMin { dst, lhs, .. }
            | MachineInst::FloatMax { dst, lhs, .. } => {
                hints.insert(*dst, *lhs);
            }
            MachineInst::IntCmov { dst, then_val, .. } => {
                hints.insert(*dst, *then_val);
            }
            MachineInst::IntNeg { dst, src }
            | MachineInst::Not { dst, src }
            | MachineInst::FloatNeg { dst, src, .. }
            | MachineInst::FloatAbs { dst, src, .. } => {
                hints.insert(*dst, *src);
            }
            _ => {}
        }
    }
    hints
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod diamond_fusion_tests {
    use super::*;
    use forge_ir::{Block, BlockData, CmpOp, Function, Inst, Terminator, Ty, Value};

    /// Mirrors Builder::emit's exact logic (push to insts/types/spans, push
    /// the index into the target block's insts) without going through
    /// Builder itself, since this file only needs the plain data shape.
    fn push_inst(func: &mut Function, block: Block, inst: Inst, ty: Ty) -> Value {
        let v = Value(func.insts.len() as u32);
        func.insts.push(inst);
        func.types.push(ty);
        func.spans.push(forge_syntax::span::Span::new(0, 0));
        func.blocks[block.0 as usize].insts.push(v);
        v
    }

    fn empty_func(num_blocks: usize) -> Function {
        Function {
            insts: Vec::new(),
            types: Vec::new(),
            spans: Vec::new(),
            blocks: vec![BlockData::default(); num_blocks],
            entry: Block(0),
            params: Vec::new(),
        }
    }

    /// Builds: entry(0) has Branch(cond, t=1, e=2); t and e are empty,
    /// both Jump to m=3; m has a Phi(t: val_t, e: val_e) plus optionally
    /// a trivial phi (same value on both edges), then Return(phi_dst).
    /// Returns (func, cond, val_t, val_e, phi_dst).
    fn build_diamond(
        val_ty: Ty,
        cmp: Option<CmpOp>,
        swap_incoming: bool,
        extra_trivial_phi: bool,
    ) -> (Function, Value, Value, Value, Value) {
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));

        let a = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 0,
                ty: val_ty,
            },
            val_ty,
        );
        let c = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 1,
                ty: val_ty,
            },
            val_ty,
        );
        let cond = if let Some(op) = cmp {
            push_inst(&mut func, entry, Inst::Cmp { op, lhs: a, rhs: c }, Ty::Bool)
        } else {
            push_inst(
                &mut func,
                entry,
                Inst::Param {
                    index: 2,
                    ty: Ty::Bool,
                },
                Ty::Bool,
            )
        };
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));

        let (val_t, val_e) = if swap_incoming { (c, a) } else { (a, c) };
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, val_t), (e, val_e)],
            },
            val_ty,
        );
        if extra_trivial_phi {
            push_inst(
                &mut func,
                m,
                Inst::Phi {
                    incoming: smallvec::smallvec![(t, a), (e, a)],
                },
                val_ty,
            );
        }
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        (func, cond, val_t, val_e, phi_dst)
    }

    #[test]
    fn eligible_diamond_is_detected_as_int_cmov() {
        let (func, cond, val_t, val_e, phi_dst) = build_diamond(Ty::I64, None, false, false);
        let (fusions, skip) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1);
        match fusions.values().next().unwrap() {
            DiamondFusion::IntCmov {
                dst,
                cond: c,
                then_val,
                else_val,
            } => {
                assert_eq!(*dst, phi_dst);
                assert_eq!(*c, cond);
                assert_eq!(*then_val, val_t);
                assert_eq!(*else_val, val_e);
            }
            other => panic!("expected IntCmov, got {other:?}"),
        }
        assert_eq!(skip.len(), 2);
    }

    #[test]
    fn non_empty_arm_is_rejected() {
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let a = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let c = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let cond = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::Bool,
            },
            Ty::Bool,
        );
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        });
        // t computes a real division -- NOT empty. This is the
        // correctness-critical near-miss: fusing this would unconditionally
        // execute IntDiv, which traps on c==0 even when cond would have
        // avoided taking this arm.
        let divided = push_inst(&mut func, t, Inst::Div(a, c), Ty::I64);
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, divided), (e, c)],
            },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(
            fusions.is_empty(),
            "a non-empty arm must never be fused -- IntDiv traps"
        );
        assert!(skip.is_empty());
    }

    #[test]
    fn multiple_phis_at_merge_only_the_differing_one_is_the_payload() {
        let (func, _cond, _val_t, _val_e, phi_dst) = build_diamond(Ty::I64, None, false, true); // extra trivial phi
        let (fusions, skip) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1, "the trivial phi must not block fusion");
        match fusions.values().next().unwrap() {
            DiamondFusion::IntCmov { dst, .. } => assert_eq!(*dst, phi_dst),
            other => panic!("expected IntCmov, got {other:?}"),
        }
        assert_eq!(skip.len(), 2);
    }

    #[test]
    fn two_differing_phis_at_merge_is_rejected() {
        // Two independent values differ between the arms -- this design's
        // single-value fusion cannot represent that; must not fuse.
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let a = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let c = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let cond = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::Bool,
            },
            Ty::Bool,
        );
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi1 = push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, a), (e, c)],
            },
            Ty::I64,
        );
        push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, c), (e, a)],
            },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi1));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty());
        assert!(skip.is_empty());
    }

    #[test]
    fn third_predecessor_of_merge_is_rejected() {
        let mut func = empty_func(5);
        let (entry, t, e, other, m) = (Block(0), Block(1), Block(2), Block(3), Block(4));
        let a = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let c = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let cond = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::Bool,
            },
            Ty::Bool,
        );
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        // `other` is an unrelated block that ALSO jumps to `m` -- a genuine
        // 3rd predecessor, unreachable from `entry` in this fixture (that's
        // fine; find_fusable_diamonds only counts real terminator edges
        // into `m`, it never requires the whole function be reachable).
        func.blocks[other.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, a), (e, c)],
            },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(
            fusions.is_empty(),
            "m has a 3rd predecessor (`other`) -- must not fuse"
        );
        assert!(skip.is_empty());
    }

    #[test]
    fn float_min_max_table_row_1_lt_a_b_is_min() {
        let (func, ..) = build_diamond(Ty::F64, Some(CmpOp::Lt), false, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax {
                op: MinMaxOp::Min, ..
            } => {}
            other => panic!("expected FloatMinMax::Min, got {other:?}"),
        }
    }

    #[test]
    fn float_min_max_table_row_2_lt_b_a_is_max_with_swapped_operands() {
        let (func, _, val_t, val_e, _) = build_diamond(Ty::F64, Some(CmpOp::Lt), true, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax {
                op: MinMaxOp::Max,
                lhs,
                rhs,
                ..
            } => {
                // The else-arm value (val_e) must be rhs; the then-arm
                // value (val_t) must be lhs -- this is the exact defect
                // execution-based design review found and fixed.
                assert_eq!(*lhs, val_t);
                assert_eq!(*rhs, val_e);
            }
            other => panic!("expected FloatMinMax::Max, got {other:?}"),
        }
    }

    #[test]
    fn float_min_max_table_row_3_gt_a_b_is_max() {
        let (func, ..) = build_diamond(Ty::F64, Some(CmpOp::Gt), false, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax {
                op: MinMaxOp::Max, ..
            } => {}
            other => panic!("expected FloatMinMax::Max, got {other:?}"),
        }
    }

    #[test]
    fn float_min_max_table_row_4_gt_b_a_is_min_with_swapped_operands() {
        let (func, _, val_t, val_e, _) = build_diamond(Ty::F64, Some(CmpOp::Gt), true, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax {
                op: MinMaxOp::Min,
                lhs,
                rhs,
                ..
            } => {
                assert_eq!(*lhs, val_t);
                assert_eq!(*rhs, val_e);
            }
            other => panic!("expected FloatMinMax::Min, got {other:?}"),
        }
    }

    #[test]
    fn float_le_ge_diamonds_produce_no_fusion_at_all() {
        let (func_le, ..) = build_diamond(Ty::F64, Some(CmpOp::Le), false, false);
        let (fusions_le, skip_le) = find_fusable_diamonds(&func_le);
        assert!(
            fusions_le.is_empty(),
            "Le must not fuse -- no derived table for it"
        );
        assert!(skip_le.is_empty());

        let (func_ge, ..) = build_diamond(Ty::F64, Some(CmpOp::Ge), false, false);
        let (fusions_ge, skip_ge) = find_fusable_diamonds(&func_ge);
        assert!(
            fusions_ge.is_empty(),
            "Ge must not fuse -- no derived table for it"
        );
        assert!(skip_ge.is_empty());
    }

    #[test]
    fn float_eq_ne_diamonds_produce_no_fusion_at_all() {
        // Closes a real gap found by mutation testing during code-quality
        // review: no test previously constructed an Eq/Ne-shaped F64
        // diamond, so a future refactor accidentally broadening the float
        // table's match guards (e.g. `Lt | Eq`) would silently misclassify
        // one as FloatMinMax with nothing to catch it.
        let (func_eq, ..) = build_diamond(Ty::F64, Some(CmpOp::Eq), false, false);
        let (fusions_eq, skip_eq) = find_fusable_diamonds(&func_eq);
        assert!(
            fusions_eq.is_empty(),
            "Eq must not fuse -- no derived table for it"
        );
        assert!(skip_eq.is_empty());

        let (func_ne, ..) = build_diamond(Ty::F64, Some(CmpOp::Ne), false, false);
        let (fusions_ne, skip_ne) = find_fusable_diamonds(&func_ne);
        assert!(
            fusions_ne.is_empty(),
            "Ne must not fuse -- no derived table for it"
        );
        assert!(skip_ne.is_empty());
    }

    #[test]
    fn float_third_value_diamond_produces_no_fusion_never_int_cmov() {
        // if a > b then a else c -- val_e (c) is NOT one of the comparison's
        // own operands, so this can't be a min/max rewrite, and it must NOT
        // fall through to IntCmov either (that path is Ty::I64/Bool only --
        // this is the exact miscompile execution-based design review found).
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let a = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::F64,
            },
            Ty::F64,
        );
        let bb = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::F64,
            },
            Ty::F64,
        );
        let cc = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::F64,
            },
            Ty::F64,
        );
        let cond = push_inst(
            &mut func,
            entry,
            Inst::Cmp {
                op: CmpOp::Gt,
                lhs: a,
                rhs: bb,
            },
            Ty::Bool,
        );
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, a), (e, cc)],
            },
            Ty::F64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(
            fusions.is_empty(),
            "float third-value diamond must produce NO fusion, never IntCmov"
        );
        assert!(skip.is_empty());
    }

    #[test]
    fn f64_diamond_with_a_non_cmp_cond_is_gated_off_the_int_cmov_path() {
        // Closes a real coverage gap execution-based plan review found by
        // mutation testing: float_third_value_diamond_produces_no_fusion
        // above never actually reaches the `ty_of(dst) != Ty::F64` hard
        // gate, because its `cond` is an Inst::Cmp over F64 operands,
        // which the EARLIER min/max-table branch already rejects (`continue`)
        // before the general IntCmov path is ever considered. Deleting the
        // hard gate entirely left every existing test passing -- this
        // fixture uses a non-Cmp cond (an ordinary bool value, matching
        // `cmp: None` in build_diamond) with F64-typed arm values, which
        // is the ONLY shape that actually exercises the gate: nothing
        // about `cond` routes it through the min/max branch at all, so
        // without the hard gate this would silently become an IntCmov
        // over Xmm-classed values -- the exact miscompile this design's
        // whole "general cmov path" section exists to prevent.
        let (func, ..) = build_diamond(Ty::F64, None, false, false);
        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty(), "F64 dst must never become an IntCmov");
        assert!(skip.is_empty());
    }

    #[test]
    fn arm_with_an_extra_predecessor_is_rejected() {
        // Found by mutation testing during plan review: deleting the
        // `pred_count[t] == 1 && pred_count[e] == 1` guard left the whole
        // test suite green -- nothing else exercises "an arm block has a
        // second, unrelated predecessor." Without the guard, `t` still
        // looks empty (rule 2 only checks t/e's own insts), so `other`'s
        // real Jump(t) target silently vanishes once t is skipped.
        let mut func = empty_func(5);
        let (entry, t, e, m, other) = (Block(0), Block(1), Block(2), Block(3), Block(4));
        let a = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let c = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::I64,
            },
            Ty::I64,
        );
        let cond = push_inst(
            &mut func,
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::Bool,
            },
            Ty::Bool,
        );
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: t,
            else_: e,
        });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[other.0 as usize].term = Some(Terminator::Jump(t));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi {
                incoming: smallvec::smallvec![(t, a), (e, c)],
            },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(
            fusions.is_empty(),
            "arm `t` has a 2nd predecessor -- must not fuse"
        );
        assert!(skip.is_empty());
    }
}
