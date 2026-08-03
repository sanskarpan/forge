// crates/forge-ir/src/dominance.rs

use rustc_hash::FxHashMap;

use crate::ir::*;

pub fn reverse_postorder(f: &Function) -> Vec<Block> {
    let mut visited = vec![false; f.blocks.len()];
    let mut post = Vec::new();
    visit(f, f.entry, &mut visited, &mut post);
    post.reverse();
    post
}

fn visit(f: &Function, b: Block, visited: &mut Vec<bool>, post: &mut Vec<Block>) {
    if visited[b.0 as usize] { return; }
    visited[b.0 as usize] = true;
    if let Some(term) = &f.blocks[b.0 as usize].term {
        match term {
            Terminator::Return(_) => {}
            Terminator::Jump(t) => visit(f, *t, visited, post),
            Terminator::Branch { then_, else_, .. } => {
                visit(f, *then_, visited, post);
                visit(f, *else_, visited, post);
            }
        }
    }
    post.push(b);
}

/// Cooper-Harvey-Kennedy iterative dominator algorithm.
pub fn compute_dominators(f: &Function) -> Vec<Option<Block>> {
    let rpo = reverse_postorder(f);
    let rpo_num: FxHashMap<Block, usize> = rpo.iter().enumerate().map(|(i, &b)| (b, i)).collect();
    let mut idom: Vec<Option<Block>> = vec![None; f.blocks.len()];
    idom[f.entry.0 as usize] = Some(f.entry);

    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo.iter().skip(1) {
            let preds = f.blocks[b.0 as usize].preds.clone();
            let mut new_idom = None;
            for p in preds {
                if idom[p.0 as usize].is_none() { continue; }
                new_idom = Some(match new_idom {
                    None => p,
                    Some(cur) => intersect(&idom, &rpo_num, cur, p),
                });
            }
            if idom[b.0 as usize] != new_idom {
                idom[b.0 as usize] = new_idom;
                changed = true;
            }
        }
    }
    idom
}

/// The "meet" operation of the Cooper-Harvey-Kennedy algorithm: given two
/// blocks that each already have a (possibly provisional) computed idom,
/// find their nearest common dominator by walking both up their own idom
/// chains in lockstep. `rpo_num` doubles as a cheap "is this an ancestor in
/// the dominator tree" proxy: a block's idom always has a strictly lower
/// RPO number than the block itself (it's visited first), so repeatedly
/// stepping the pointer with the *larger* RPO number to its own idom moves
/// that pointer strictly toward the entry — when both pointers finally
/// agree, that block is where `a`'s and `b`'s idom chains first converge,
/// i.e. their nearest common dominator.
fn intersect(idom: &[Option<Block>], rpo_num: &FxHashMap<Block, usize>, mut a: Block, mut b: Block) -> Block {
    while a != b {
        while rpo_num[&a] > rpo_num[&b] { a = idom[a.0 as usize].expect("reachable block has an idom"); }
        while rpo_num[&b] > rpo_num[&a] { b = idom[b.0 as usize].expect("reachable block has an idom"); }
    }
    a
}

pub fn dominates(idom: &[Option<Block>], a: Block, mut b: Block) -> bool {
    loop {
        if a == b { return true; }
        match idom[b.0 as usize] {
            Some(next) if next != b => b = next,
            _ => return false,
        }
    }
}
