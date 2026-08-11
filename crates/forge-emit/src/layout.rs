use forge_ir::{Function, Value};
use forge_regalloc::Location;
use forge_x64::SelectedFunction;
use std::collections::HashMap;

pub fn emit_body(
    _func: &Function,
    _selected: &SelectedFunction,
    _assignment: &HashMap<Value, Location>,
) -> Vec<u8> {
    unimplemented!("filled in by Task 6")
}
