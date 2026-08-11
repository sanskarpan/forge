use forge_x64::{Assembler, ConstantPool, Label};

pub fn alloc_pool_labels(asm: &mut Assembler, pool: &ConstantPool) -> Vec<Label> {
    (0..pool.entries().len()).map(|_| asm.new_label()).collect()
}

pub fn place_pool(asm: &mut Assembler, pool: &ConstantPool, labels: &[Label]) {
    for (&bits, &label) in pool.entries().iter().zip(labels) {
        asm.bind(label);
        asm.emit_u64(bits);
    }
}
