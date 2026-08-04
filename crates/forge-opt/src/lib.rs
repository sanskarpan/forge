// crates/forge-opt/src/lib.rs

pub mod fold;
pub mod simplify;
pub mod strength;

use forge_ir::Function;

/// One optimization transform. `run` returns whether it changed the IR —
/// the driver uses this to detect a fixed point.
pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&mut self, f: &mut Function) -> bool;
}

/// Runs `passes` in order, repeating until none of them report a change
/// (fixed point) or 10 rounds pass, whichever comes first. Re-verifies the
/// IR after EVERY individual pass in debug builds — not just once at the
/// end — per CHECKLIST.md: "catches an optimizer bug at the pass that
/// caused it, not three passes later."
pub fn run_passes(f: &mut Function, passes: &mut [Box<dyn Pass>]) {
    for round in 0..10 {
        let mut changed = false;
        for pass in passes.iter_mut() {
            let pass_changed = pass.run(f);
            changed |= pass_changed;
            #[cfg(debug_assertions)]
            if let Err(e) = forge_ir::verify::verify(f) {
                panic!(
                    "verifier failed after pass '{}' (round {round}): {e}",
                    pass.name()
                );
            }
        }
        if !changed {
            break;
        }
    }
}

/// The real optimization pipeline. Empty for now — later tasks each add one
/// `Box::new(TheirPass)` to this vec as their pass lands, in the order
/// SPEC.md §6.5 specifies: fold, simplify, strength-reduce, GVN, reassoc, DCE.
pub fn optimize(f: &mut Function) {
    let mut passes: Vec<Box<dyn Pass>> = vec![
        Box::new(fold::ConstFold),
        Box::new(simplify::AlgebraicSimplify),
        Box::new(strength::StrengthReduceShifts),
        // Box::new(strength::MagicDivision), <- later task
        // Box::new(gvn::Gvn),                     <- added by a later task
        // Box::new(reassoc::Reassociate),          <- added by a later task (before DCE — reassoc can expose new DCE opportunities)
        // Box::new(dce::Dce),                     <- added by a later task
    ];
    run_passes(f, &mut passes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::*;
    use forge_syntax::span::Span;
    use std::cell::Cell;
    use std::rc::Rc;

    fn trivial_function() -> Function {
        Function {
            insts: vec![Inst::Param {
                index: 0,
                ty: Ty::F64,
            }],
            types: vec![Ty::F64],
            spans: vec![Span::new(0, 0)],
            blocks: vec![BlockData {
                insts: vec![Value(0)],
                term: Some(Terminator::Return(Value(0))),
                preds: Default::default(),
            }],
            entry: Block(0),
            params: vec![("x".to_string(), Ty::F64)],
        }
    }

    struct CountingPass {
        calls: Rc<Cell<u32>>,
        fire_times: u32,
    }
    impl Pass for CountingPass {
        fn name(&self) -> &'static str {
            "counting-test-pass"
        }
        fn run(&mut self, _f: &mut Function) -> bool {
            let n = self.calls.get();
            self.calls.set(n + 1);
            n < self.fire_times
        }
    }

    #[test]
    fn driver_runs_to_fixed_point_and_stops() {
        let mut f = trivial_function();
        let calls = Rc::new(Cell::new(0));
        let mut passes: Vec<Box<dyn Pass>> = vec![Box::new(CountingPass {
            calls: calls.clone(),
            fire_times: 3,
        })];
        run_passes(&mut f, &mut passes);
        // Pass reports "changed" on calls 0,1,2 (3 times), then false on
        // call 3 -> the round-3 call is what makes changed=false, ending
        // the loop after that same round. Total calls: 4.
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn driver_caps_at_ten_rounds_even_if_always_changed() {
        let mut f = trivial_function();
        let calls = Rc::new(Cell::new(0));
        let mut passes: Vec<Box<dyn Pass>> = vec![Box::new(CountingPass {
            calls: calls.clone(),
            fire_times: u32::MAX,
        })];
        run_passes(&mut f, &mut passes);
        assert_eq!(calls.get(), 10);
    }

    struct BreakingPass;
    impl Pass for BreakingPass {
        fn name(&self) -> &'static str {
            "breaking-test-pass"
        }
        fn run(&mut self, f: &mut Function) -> bool {
            // Deliberately corrupt: point Return at a value that doesn't exist.
            f.blocks[0].term = Some(Terminator::Return(Value(999)));
            true
        }
    }

    #[test]
    #[should_panic(expected = "breaking-test-pass")]
    fn verifier_runs_after_every_pass_and_names_the_culprit() {
        let mut f = trivial_function();
        let mut passes: Vec<Box<dyn Pass>> = vec![Box::new(BreakingPass)];
        run_passes(&mut f, &mut passes);
    }

    #[test]
    fn empty_pipeline_leaves_function_unchanged() {
        let mut f = trivial_function();
        optimize(&mut f);
        assert_eq!(f.insts.len(), 1);
    }
}
