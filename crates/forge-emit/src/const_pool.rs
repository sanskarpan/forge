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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_pool_writes_entries_in_order_after_existing_code() {
        let mut pool = ConstantPool::default();
        let a = pool.intern(0x3ff0000000000000u64); // 1.0f64
        let b = pool.intern(0x4000000000000000u64); // 2.0f64
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);

        let mut asm = Assembler::new();
        asm.ret(); // 1 byte of "existing code" (0xC3), so the pool isn't at offset 0
        let labels = alloc_pool_labels(&mut asm, &pool);
        assert_eq!(labels.len(), 2);
        place_pool(&mut asm, &pool, &labels);

        let code = asm.code();
        assert_eq!(code.len(), 1 + 16); // 1 ret byte + two 8-byte pool entries
        assert_eq!(code[0], 0xC3);
        assert_eq!(&code[1..9], &0x3ff0000000000000u64.to_le_bytes());
        assert_eq!(&code[9..17], &0x4000000000000000u64.to_le_bytes());
    }

    #[test]
    fn empty_pool_produces_no_labels_and_no_bytes() {
        let pool = ConstantPool::default();
        let mut asm = Assembler::new();
        let labels = alloc_pool_labels(&mut asm, &pool);
        assert!(labels.is_empty());
        place_pool(&mut asm, &pool, &labels);
        assert!(asm.code().is_empty());
    }
}
