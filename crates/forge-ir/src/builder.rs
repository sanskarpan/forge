// crates/forge-ir/src/builder.rs

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::ir::*;

pub struct Builder {
    pub f: Function,
    current_def: Vec<FxHashMap<String, Value>>,
    sealed: Vec<bool>,
    incomplete_phis: Vec<FxHashMap<String, Value>>,
    pub cur_block: Block,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            f: Function {
                insts: Vec::new(), types: Vec::new(), spans: Vec::new(),
                blocks: Vec::new(), entry: Block(0), params: Vec::new(),
            },
            current_def: Vec::new(),
            sealed: Vec::new(),
            incomplete_phis: Vec::new(),
            cur_block: Block(0),
        }
    }

    pub fn create_block(&mut self) -> Block {
        let id = Block(self.f.blocks.len() as u32);
        self.f.blocks.push(BlockData::default());
        self.current_def.push(FxHashMap::default());
        self.sealed.push(false);
        self.incomplete_phis.push(FxHashMap::default());
        id
    }

    pub fn add_pred(&mut self, block: Block, pred: Block) {
        self.f.blocks[block.0 as usize].preds.push(pred);
    }

    pub fn seal_block(&mut self, block: Block) {
        let pending: Vec<(String, Value)> = self.incomplete_phis[block.0 as usize].drain().collect();
        for (name, phi) in pending {
            self.fill_phi_operands(phi, block, &name);
        }
        self.sealed[block.0 as usize] = true;
    }

    pub fn emit(&mut self, block: Block, inst: Inst, ty: Ty, span: forge_syntax::span::Span) -> Value {
        let v = Value(self.f.insts.len() as u32);
        self.f.insts.push(inst);
        self.f.types.push(ty);
        self.f.spans.push(span);
        self.f.blocks[block.0 as usize].insts.push(v);
        v
    }

    fn new_phi(&mut self, block: Block, ty: Ty) -> Value {
        self.emit(block, Inst::Phi { incoming: SmallVec::new() }, ty, forge_syntax::span::Span::new(0, 0))
    }

    pub fn write_variable(&mut self, name: &str, block: Block, value: Value) {
        self.current_def[block.0 as usize].insert(name.to_string(), value);
    }

    pub fn read_variable(&mut self, name: &str, block: Block, ty: Ty) -> Value {
        if let Some(&v) = self.current_def[block.0 as usize].get(name) { return v; }
        self.read_variable_recursive(name, block, ty)
    }

    fn read_variable_recursive(&mut self, name: &str, block: Block, ty: Ty) -> Value {
        if !self.sealed[block.0 as usize] {
            let phi = self.new_phi(block, ty);
            self.incomplete_phis[block.0 as usize].insert(name.to_string(), phi);
            self.write_variable(name, block, phi);
            return phi;
        }
        let preds = self.f.blocks[block.0 as usize].preds.clone();
        if preds.len() == 1 {
            let v = self.read_variable(name, preds[0], ty);
            self.write_variable(name, block, v);
            return v;
        }
        let phi = self.new_phi(block, ty);
        self.write_variable(name, block, phi); // break cycles before recursing into preds
        self.fill_phi_operands(phi, block, name);
        phi
    }

    fn fill_phi_operands(&mut self, phi: Value, block: Block, name: &str) {
        let ty = self.f.types[phi.0 as usize];
        let preds = self.f.blocks[block.0 as usize].preds.clone();
        let mut incoming = SmallVec::new();
        for p in preds {
            let v = self.read_variable(name, p, ty);
            incoming.push((p, v));
        }
        if let Inst::Phi { incoming: slot } = &mut self.f.insts[phi.0 as usize] {
            *slot = incoming;
        }
        self.try_remove_trivial_phi(phi);
    }

    /// A phi whose operands are all the same value (ignoring itself) is
    /// redundant — replace its uses with that value. This is what keeps
    /// nested-`if` lowering from leaving dead phis behind.
    fn try_remove_trivial_phi(&mut self, phi: Value) {
        let incoming = match &self.f.insts[phi.0 as usize] {
            Inst::Phi { incoming } => incoming.clone(),
            _ => return,
        };
        let mut same: Option<Value> = None;
        for (_, v) in &incoming {
            if *v == phi { continue; }
            match same {
                Some(s) if s != *v => return,
                _ => same = Some(*v),
            }
        }
        if let Some(replacement) = same {
            self.replace_all_uses(phi, replacement);
        }
    }

    pub fn replace_all_uses(&mut self, old: Value, new: Value) {
        for inst in &mut self.f.insts {
            replace_in_inst(inst, old, new);
        }
        for def in &mut self.current_def {
            for v in def.values_mut() {
                if *v == old { *v = new; }
            }
        }
    }
}

impl Default for Builder {
    fn default() -> Self { Self::new() }
}
