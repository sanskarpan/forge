use forge_ir::Value;
use forge_x64::{Assembler, Label, MachineInst, PhysReg};

pub fn translate_inst(
    _asm: &mut Assembler,
    inst: &MachineInst,
    _loc: &dyn Fn(Value) -> PhysReg,
    _pool_labels: &[Label],
) {
    unimplemented!("filled in by Task 4/5: {inst:?}")
}
