use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};
use std::collections::HashMap;

/// An index into a ConstantPool's entries. Opaque outside this module,
/// same "not usable across a different pool" caveat as Label is for a
/// different Assembler (Phase 6a's precedent).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PoolIndex(usize);

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

pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_scaled_indices = find_fully_fusable_scaled_indices(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_scaled_indices,
        pool: ConstantPool::default(),
    };
    let mut block_starts = Vec::new();
    for block in forge_ir::dominance::reverse_postorder(func) {
        block_starts.push((block, sel.insts.len()));
        for &v in &func.blocks[block.0 as usize].insts {
            let inst = &func.insts[v.0 as usize];
            sel.select_inst(v, inst);
        }
        if let Some(term) = &func.blocks[block.0 as usize].term {
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
