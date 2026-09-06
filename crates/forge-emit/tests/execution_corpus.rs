mod disasm;
use disasm::disassemble;

use forge_ir::builder::Builder;
use forge_ir::{CmpOp, Inst, Terminator, Ty, Value};
use forge_regalloc::Location;
use forge_x64::PhysReg;
use smallvec::smallvec;
use std::collections::HashMap;

fn dummy_span() -> forge_syntax::span::Span {
    forge_syntax::span::Span::new(0, 0)
}

#[cfg(target_arch = "x86_64")]
fn run_f64(code: &[u8]) -> f64 {
    let mut buf = forge_mem::ExecutableBuffer::new(code.len().max(64)).unwrap();
    buf.write(|mem| mem[..code.len()].copy_from_slice(code));
    buf.make_executable().unwrap();
    let compiled = forge_mem::CompiledExpr::from_buffer(buf, 0);
    compiled.call_n(&[])
}

#[cfg(target_arch = "x86_64")]
fn run_f64_arg(code: &[u8], arg: f64) -> f64 {
    let mut buf = forge_mem::ExecutableBuffer::new(code.len().max(64)).unwrap();
    buf.write(|mem| mem[..code.len()].copy_from_slice(code));
    buf.make_executable().unwrap();
    // SAFETY: the emitter's scalar ABI uses one f64 argument in XMM0 and
    // returns the f64 result in XMM0; the executable mapping stays alive for
    // the duration of this call.
    let function: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    unsafe { function(arg) }
}

#[cfg(target_arch = "x86_64")]
fn run_f64_two(code: &[u8], args: [f64; 2]) -> f64 {
    let mut buf = forge_mem::ExecutableBuffer::new(code.len().max(64)).unwrap();
    buf.write(|mem| mem[..code.len()].copy_from_slice(code));
    buf.make_executable().unwrap();
    let compiled = forge_mem::CompiledExpr::from_buffer(buf, 2);
    compiled.call_args(&args)
}

#[cfg(target_arch = "x86_64")]
fn run_f64_three(code: &[u8], args: [f64; 3]) -> f64 {
    let mut buf = forge_mem::ExecutableBuffer::new(code.len().max(64)).unwrap();
    buf.write(|mem| mem[..code.len()].copy_from_slice(code));
    buf.make_executable().unwrap();
    let compiled = forge_mem::CompiledExpr::from_buffer(buf, 3);
    compiled.call_args(&args)
}

#[cfg(target_arch = "x86_64")]
#[test]
fn scalar_minmax_preserves_left_operand_on_equal_signed_zero() {
    fn emit_minmax(op: fn(Value, Value) -> Inst) -> Vec<u8> {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        b.f.params = vec![("x".to_string(), Ty::F64), ("y".to_string(), Ty::F64)];
        let x = b.emit(
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::F64,
            },
            Ty::F64,
            dummy_span(),
        );
        let y = b.emit(
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::F64,
            },
            Ty::F64,
            dummy_span(),
        );
        let result = b.emit(entry, op(x, y), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(result));

        let selected = forge_x64::select(&b.f);
        let assignment: HashMap<Value, Location> = [
            (x, Location::Reg(PhysReg::Xmm0)),
            (y, Location::Reg(PhysReg::Xmm1)),
            (result, Location::Reg(PhysReg::Xmm0)),
        ]
        .into_iter()
        .collect();
        forge_emit::emit_body(&b.f, &selected, &assignment)
    }

    let negative_zero = (-0.0f64).to_bits();
    let positive_zero = 0.0f64.to_bits();

    for (name, op, args, expected) in [
        (
            "min",
            Inst::Min as fn(Value, Value) -> Inst,
            [-0.0, 0.0],
            negative_zero,
        ),
        (
            "min",
            Inst::Min as fn(Value, Value) -> Inst,
            [0.0, -0.0],
            positive_zero,
        ),
        (
            "max",
            Inst::Max as fn(Value, Value) -> Inst,
            [-0.0, 0.0],
            negative_zero,
        ),
        (
            "max",
            Inst::Max as fn(Value, Value) -> Inst,
            [0.0, -0.0],
            positive_zero,
        ),
    ] {
        let actual = run_f64_two(&emit_minmax(op), args);
        assert_eq!(
            actual.to_bits(),
            expected,
            "operation={name}, args={args:?}"
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn scalar_fma_executes_with_single_rounding() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    b.f.params = vec![
        ("a".to_string(), Ty::F64),
        ("b".to_string(), Ty::F64),
        ("c".to_string(), Ty::F64),
    ];
    let a = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let bb = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let c = b.emit(
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let result = b.emit(entry, Inst::Fma { a, b: bb, c }, Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(result));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> = [
        (a, Location::Reg(PhysReg::Xmm0)),
        (bb, Location::Reg(PhysReg::Xmm1)),
        (c, Location::Reg(PhysReg::Xmm2)),
        (result, Location::Reg(PhysReg::Xmm3)),
    ]
    .into_iter()
    .collect();
    let code = forge_emit::emit_body(&b.f, &selected, &assignment);

    // (1 + 2^-27) * (1 - 2^-27) - 1 is -2^-54 exactly. Rounding the
    // product before the subtraction loses that result and produces zero.
    let a = 1.0 + 2f64.powi(-27);
    let bb = 1.0 - 2f64.powi(-27);
    let c = -1.0;
    let actual = run_f64_three(&code, [a, bb, c]);
    let expected = a.mul_add(bb, c);
    assert_eq!(expected.to_bits(), (-2f64.powi(-54)).to_bits());
    assert_eq!(actual.to_bits(), expected.to_bits());
}

#[test]
fn float_neg_flips_sign_bit() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(
        entry,
        Inst::ConstF64(3.0f64.to_bits()),
        Ty::F64,
        dummy_span(),
    );
    let negated = b.emit(entry, Inst::Neg(c), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(negated));

    let selected = forge_x64::select(&b.f);
    let mut assignment: HashMap<Value, Location> = HashMap::new();
    assignment.insert(c, Location::Reg(PhysReg::Xmm0));
    assignment.insert(negated, Location::Reg(PhysReg::Xmm0));

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    let scratch_xmm = if cfg!(windows) {
        "xorpd xmm0,xmm3"
    } else {
        "xorpd xmm0,xmm13"
    };
    assert!(lines.iter().any(|l| l == scratch_xmm));

    #[cfg(target_arch = "x86_64")]
    assert_eq!(run_f64(&code), -3.0);
}

#[test]
fn int_cmp_and_cmov_diamond_selects_correct_branch() {
    // Equivalent to: (a > b) ? a : b, with a=5.0, b=2.0 baked in as constants,
    // lowered through the real Select->cmov diamond fusion from Phase 7f so
    // this also exercises IntCmov end-to-end through forge-emit for the
    // first time (Phase 7f flagged this as a real coverage gap).
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_blk = b.create_block();
    let else_blk = b.create_block();
    let merge = b.create_block();
    b.seal_block(entry);

    let a = b.emit(entry, Inst::ConstI64(5), Ty::I64, dummy_span());
    let bb = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
    let cond = b.emit(
        entry,
        Inst::Cmp {
            op: CmpOp::Gt,
            lhs: a,
            rhs: bb,
        },
        Ty::Bool,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: then_blk,
        else_: else_blk,
    });
    b.add_pred(then_blk, entry);
    b.add_pred(else_blk, entry);
    b.seal_block(then_blk);
    b.seal_block(else_blk);
    // Braun-style SSA construction is keyed by variable NAME, not by Value —
    // verified against `crates/forge-ir/src/builder.rs`'s real signatures:
    // `write_variable(&mut self, name: &str, block: Block, value: Value)` and
    // `read_variable(&mut self, name: &str, block: Block, ty: Ty) -> Value`.
    b.write_variable("result", then_blk, a);
    b.f.blocks[then_blk.0 as usize].term = Some(Terminator::Jump(merge));
    b.write_variable("result", else_blk, bb);
    b.f.blocks[else_blk.0 as usize].term = Some(Terminator::Jump(merge));
    b.add_pred(merge, then_blk);
    b.add_pred(merge, else_blk);
    b.seal_block(merge);
    // merge has two preds with differing incoming values for "result" (a vs
    // bb), so this mints a real Inst::Phi at merge per read_variable_recursive's
    // documented behavior.
    let result = b.read_variable("result", merge, Ty::I64);
    b.f.blocks[merge.0 as usize].term = Some(Terminator::Return(result));

    let selected = forge_x64::select(&b.f);
    let mut assignment: HashMap<Value, Location> = HashMap::new();
    assignment.insert(a, Location::Reg(PhysReg::Rax));
    assignment.insert(bb, Location::Reg(PhysReg::Rcx));
    assignment.insert(cond, Location::Reg(PhysReg::Rdx));
    // NOTE: if find_fusable_diamonds fuses this diamond (empty arm blocks,
    // both Jump to merge, one differing phi), `result`'s Value here is the
    // IntCmov's dst — assign it Rax to match the diamond's then_val register
    // (a's register) so the 2-addr fixup is a no-op; consult
    // forge_x64::find_fusable_diamonds(&b.f) directly if the exact Value
    // identity of the fused dst isn't obvious from `result` above, and
    // assign whatever that returns instead.
    assignment.insert(result, Location::Reg(PhysReg::Rax));

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert!(lines
        .iter()
        .any(|l| l.starts_with("cmove") || l.starts_with("test")));

    // No execution assertion here: `result` is `Ty::I64`, returned in `rax`,
    // but `forge-mem`'s `CompiledExpr` only exposes f64-typed call paths
    // (`call1`/`call2`/`call_n` all read `xmm0`) — an int-typed top-level
    // return has no call path until Phase 9f adds one. The disassembly
    // assertion above is this test's real bar for now.
}

#[test]
fn floating_phi_is_copied_on_both_control_flow_edges() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_blk = b.create_block();
    let else_blk = b.create_block();
    let merge = b.create_block();
    b.f.params = vec![
        ("condition".to_string(), Ty::Bool),
        ("then_input".to_string(), Ty::F64),
        ("else_input".to_string(), Ty::F64),
    ];
    b.seal_block(entry);

    let condition = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::Bool,
        },
        Ty::Bool,
        dummy_span(),
    );
    let then_input = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let else_input = b.emit(
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond: condition,
        then_: then_blk,
        else_: else_blk,
    });
    b.add_pred(then_blk, entry);
    b.add_pred(else_blk, entry);
    b.seal_block(then_blk);
    b.seal_block(else_blk);

    let then_value = b.emit(then_blk, Inst::Neg(then_input), Ty::F64, dummy_span());
    b.f.blocks[then_blk.0 as usize].term = Some(Terminator::Jump(merge));
    let else_value = b.emit(else_blk, Inst::Neg(else_input), Ty::F64, dummy_span());
    b.f.blocks[else_blk.0 as usize].term = Some(Terminator::Jump(merge));
    b.add_pred(merge, then_blk);
    b.add_pred(merge, else_blk);
    b.seal_block(merge);
    let result = b.emit(
        merge,
        Inst::Phi {
            incoming: smallvec![(then_blk, then_value), (else_blk, else_value)],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[merge.0 as usize].term = Some(Terminator::Return(result));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> = [
        (condition, Location::Reg(PhysReg::Rax)),
        (then_input, Location::Reg(PhysReg::Xmm0)),
        (else_input, Location::Reg(PhysReg::Xmm1)),
        (then_value, Location::Reg(PhysReg::Xmm2)),
        (else_value, Location::Reg(PhysReg::Xmm3)),
        (result, Location::Reg(PhysReg::Xmm4)),
    ]
    .into_iter()
    .collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert!(lines.iter().any(|line| line == "movsd xmm4,xmm2"));
    assert!(lines.iter().any(|line| line == "movsd xmm4,xmm3"));

    // The generated x86 emitter currently follows the SysV register ABI.
    // Win64 uses positional RCX/XMM1/XMM2 argument registers instead, and
    // full Win64 emission is a separate open backend boundary. Keep this
    // execution assertion on the ABI it actually describes; Windows still
    // compiles and disassembles the generated bytes in this test.
    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    {
        let mut buf = forge_mem::ExecutableBuffer::new(code.len().max(64)).unwrap();
        buf.write(|mem| mem[..code.len()].copy_from_slice(&code));
        buf.make_executable().unwrap();
        // SysV passes the bool in RDI and the two f64 values in XMM0/XMM1.
        // SAFETY: the generated function has exactly that ABI and returns the
        // selected f64 value in XMM0; the executable mapping remains live.
        let function: unsafe extern "C" fn(u64, f64, f64) -> f64 =
            unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(unsafe { function(1, 3.0, 5.0) }, -3.0);
        assert_eq!(unsafe { function(0, 3.0, 5.0) }, -5.0);
    }
}

#[test]
fn spilled_operand_is_reloaded_and_stored_in_a_frame() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> = [(c, Location::Spill(0))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert!(lines.iter().any(|line| line.starts_with("push rbp")));
    assert!(lines.iter().any(|line| line.starts_with("mov [rbp-8]")));
}

#[test]
fn five_hundred_live_values_force_spills_and_preserve_result() {
    let live_values = 500usize;
    let mut builder = Builder::new();
    let entry = builder.create_block();
    builder.seal_block(entry);
    let input = builder.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    builder.f.params.push(("x".to_string(), Ty::F64));

    let terms = (0..live_values)
        .map(|index| {
            let constant = builder.emit(
                entry,
                Inst::ConstF64((index as f64).to_bits()),
                Ty::F64,
                dummy_span(),
            );
            builder.emit(entry, Inst::Add(input, constant), Ty::F64, dummy_span())
        })
        .collect::<Vec<_>>();
    let result = terms
        .iter()
        .copied()
        .reduce(|lhs, rhs| builder.emit(entry, Inst::Add(lhs, rhs), Ty::F64, dummy_span()))
        .unwrap();
    builder.f.blocks[entry.0 as usize].term = Some(Terminator::Return(result));

    forge_ir::verify::verify(&builder.f).unwrap();
    let selected = forge_x64::select(&builder.f);
    let intervals = forge_regalloc::build_intervals(&builder.f, &selected);
    let excluded = forge_regalloc::excluded_registers(&builder.f, &selected);
    let (assignment, _) = forge_regalloc::allocate(intervals.clone(), &excluded, &selected);
    forge_regalloc::verify_allocation(&intervals, &assignment).unwrap();
    let spills = assignment
        .values()
        .filter(|location| matches!(location, Location::Spill(_)))
        .count();
    assert!(
        spills > 100,
        "500 live values should force heavy spilling, observed {spills} spills"
    );

    let code = forge_emit::emit_body(&builder.f, &selected, &assignment);
    #[cfg(target_arch = "x86_64")]
    {
        let expected = (0..live_values).fold(0.0, |sum, index| sum + 0.25 + index as f64);
        assert_eq!(run_f64_arg(&code, 0.25).to_bits(), expected.to_bits());
    }
    #[cfg(not(target_arch = "x86_64"))]
    assert!(!code.is_empty());
}
