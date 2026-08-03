// crates/forge-ir/src/print.rs

use std::fmt::Write;

use crate::ir::*;

pub fn print_function(f: &Function) -> String {
    let mut out = String::new();
    // Block labels are only emitted when there's more than one block: with a
    // single straight-line block (the common case — no branches to target),
    // the label is pure noise, so the printer's "one line per instruction
    // plus terminator" contract holds exactly rather than off-by-one.
    let multi_block = f.blocks.len() > 1;
    for (bi, bd) in f.blocks.iter().enumerate() {
        if multi_block {
            writeln!(out, "block{bi}:").unwrap();
        }
        for &v in &bd.insts {
            writeln!(out, "  v{} = {}", v.0, print_inst(&f.insts[v.0 as usize])).unwrap();
        }
        match &bd.term {
            Some(Terminator::Return(v)) => writeln!(out, "  ret v{}", v.0).unwrap(),
            Some(Terminator::Jump(b)) => writeln!(out, "  jump block{}", b.0).unwrap(),
            Some(Terminator::Branch { cond, then_, else_ }) =>
                writeln!(out, "  branch v{}, block{}, block{}", cond.0, then_.0, else_.0).unwrap(),
            None => writeln!(out, "  <no terminator>").unwrap(),
        }
    }
    out
}

fn print_inst(inst: &Inst) -> String {
    match inst {
        Inst::ConstF64(bits) => format!("const.f64 {}", f64::from_bits(*bits)),
        Inst::ConstI64(n) => format!("const.i64 {n}"),
        Inst::ConstBool(v) => format!("const.bool {v}"),
        Inst::Param { index, ty } => format!("param {index} : {ty:?}"),
        Inst::Add(a, b) => format!("add v{}, v{}", a.0, b.0),
        Inst::Sub(a, b) => format!("sub v{}, v{}", a.0, b.0),
        Inst::Mul(a, b) => format!("mul v{}, v{}", a.0, b.0),
        Inst::Div(a, b) => format!("div v{}, v{}", a.0, b.0),
        Inst::Rem(a, b) => format!("rem v{}, v{}", a.0, b.0),
        Inst::Neg(a) => format!("neg v{}", a.0),
        Inst::Fma { a, b, c } => format!("fma v{}, v{}, v{}", a.0, b.0, c.0),
        Inst::And(a, b) => format!("and v{}, v{}", a.0, b.0),
        Inst::Or(a, b) => format!("or v{}, v{}", a.0, b.0),
        Inst::Xor(a, b) => format!("xor v{}, v{}", a.0, b.0),
        Inst::Not(a) => format!("not v{}", a.0),
        Inst::Shl(a, b) => format!("shl v{}, v{}", a.0, b.0),
        Inst::Shr(a, b) => format!("shr v{}, v{}", a.0, b.0),
        Inst::Sar(a, b) => format!("sar v{}, v{}", a.0, b.0),
        Inst::Cmp { op, lhs, rhs } => format!("cmp.{op:?} v{}, v{}", lhs.0, rhs.0),
        Inst::Sqrt(a) => format!("sqrt v{}", a.0),
        Inst::Abs(a) => format!("abs v{}", a.0),
        Inst::Min(a, b) => format!("min v{}, v{}", a.0, b.0),
        Inst::Max(a, b) => format!("max v{}, v{}", a.0, b.0),
        Inst::Floor(a) => format!("floor v{}", a.0),
        Inst::Ceil(a) => format!("ceil v{}", a.0),
        Inst::Round(a) => format!("round v{}", a.0),
        Inst::Trunc(a) => format!("trunc v{}", a.0),
        Inst::Call { func, args } => {
            let parts: Vec<String> = args.iter().map(|a| format!("v{}", a.0)).collect();
            format!("call.{func:?} {}", parts.join(", "))
        }
        Inst::IToF(a) => format!("itof v{}", a.0),
        Inst::FToI(a) => format!("ftoi v{}", a.0),
        Inst::Phi { incoming } => {
            let parts: Vec<String> = incoming.iter().map(|(b, v)| format!("block{} -> v{}", b.0, v.0)).collect();
            format!("phi [{}]", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    #[test]
    fn prints_one_line_per_instruction_plus_terminator() {
        let (tokens, _) = lex("sqrt(x * x + y * y)");
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).unwrap();
        let f = crate::lower::lower(&typed);
        let text = print_function(&f);
        // 2 params + mul + mul + add + sqrt = 6 inst lines, + 1 ret line.
        assert_eq!(text.lines().count(), 7);
        assert!(text.contains("sqrt"));
        assert!(text.contains("ret"));
    }
}