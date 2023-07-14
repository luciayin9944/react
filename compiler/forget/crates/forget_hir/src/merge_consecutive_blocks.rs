use std::collections::HashMap;

use forget_diagnostics::{invariant, Diagnostic};
use thiserror::Error;

use crate::{
    mark_instruction_ids, mark_predecessors, BasicBlock, BlockId, BlockKind, BlockRewriter,
    BlockRewriterAction, Environment, Function, IdentifierOperand, InstrIx, Instruction,
    InstructionKind, InstructionValue, LValue, LoadLocal, Operand, StoreLocal, TerminalValue,
};

/// Merges sequences of blocks that will always execute consecutively —
/// ie where the predecessor always transfers control to the successor
/// (ends in a goto) and where the predecessor is the only predecessor
/// for that successor (ie, there is no other way to reach the successor).
///
/// Note that this pass leaves value/loop blocks alone because they cannot
/// be merged without breaking the structure of the high-level terminals
/// that reference them.
pub fn merge_consecutive_blocks<'a>(
    env: &Environment<'a>,
    fun: &mut Function<'a>,
) -> Result<(), Diagnostic> {
    let mut merged = MergedBlocks::default();
    let blocks = &mut fun.body.blocks;
    let instructions = &mut fun.body.instructions;
    let mut rewriter = BlockRewriter::new(blocks, fun.body.entry);
    let mut has_changes = false;

    rewriter.try_each_block(|mut block, rewriter| {
        let block_id = block.id;
        // Visit instructions to merge blocks within function expressions
        for instr_ix in &block.instructions {
            let instr = &mut instructions[usize::from(*instr_ix)];
            if let InstructionValue::Function(fun) = &mut instr.value {
                merge_consecutive_blocks(env, &mut fun.lowered_function)?;
            }
        }

        // Can't merge value blocks and can't merge blocks with multiple
        // predecessors
        if block.kind != BlockKind::Block || block.predecessors.len() != 1 {
            return Ok(BlockRewriterAction::Keep(block));
        }

        let original_predecessor_id = block.predecessors.first().unwrap(); // length checked above
        let predecessor_id = merged.get(*original_predecessor_id);
        let predecessor = rewriter.block_mut(predecessor_id);
        if predecessor.kind != BlockKind::Block
            || !matches!(predecessor.terminal.value, TerminalValue::Goto(_))
        {
            // Can't merge value blocks, and we can't merge if the predecessor
            // has multiple successors (and isn't guaranteed to transfer here)
            return Ok(BlockRewriterAction::Keep(block));
        }

        // Replace phis in the merged block with canonical assignments to the single
        // operand value
        for phi in block.phis.iter_mut() {
            invariant(phi.operands.len() == 1, || {
                Diagnostic::invariant(ExpectedSingleOperandPhis { block: block_id }, None)
            })?;
            let (_, operand) = phi.operands.first().unwrap();
            // load the operand
            let load = Instruction {
                id: predecessor.terminal.id,
                value: InstructionValue::LoadLocal(LoadLocal {
                    place: IdentifierOperand {
                        effect: None,
                        identifier: operand.clone(),
                    },
                }),
            };
            let load_ix = InstrIx::new(instructions.len() as u32);
            instructions.push(load);
            predecessor.instructions.push(load_ix);
            // store it into the phi id
            let store = Instruction {
                id: predecessor.terminal.id,
                value: InstructionValue::StoreLocal(StoreLocal {
                    lvalue: LValue {
                        kind: InstructionKind::Reassign,
                        identifier: IdentifierOperand {
                            identifier: phi.identifier.clone(),
                            effect: None,
                        },
                    },
                    value: Operand {
                        effect: None,
                        ix: load_ix,
                    },
                }),
            };
            let store_ix = InstrIx::new(instructions.len() as u32);
            instructions.push(store);
            predecessor.instructions.push(store_ix);
        }
        let BasicBlock {
            instructions,
            terminal,
            ..
        } = *block;
        predecessor.instructions.extend(instructions);
        predecessor.terminal = terminal;
        merged.merge(block_id, predecessor_id);

        has_changes = true;
        Ok(BlockRewriterAction::Remove)
    })?;

    if has_changes {
        mark_instruction_ids(&mut fun.body)?;
        mark_predecessors(&mut fun.body);
    }

    Ok(())
}

#[derive(Default)]
struct MergedBlocks {
    merged: HashMap<BlockId, BlockId>,
}

impl MergedBlocks {
    fn merge(&mut self, block: BlockId, into: BlockId) {
        let target = self.get(into);
        self.merged.insert(block, target);
    }

    fn get(&self, block: BlockId) -> BlockId {
        let mut current = block;
        while let Some(mapped) = self.merged.get(&current) {
            current = *mapped;
        }
        current
    }
}

#[derive(Debug, Error)]
#[error("Expected predecessor {predecessor} to exist")]
pub struct ExpectedPredecessorToExist {
    predecessor: BlockId,
}

#[derive(Debug, Error)]
#[error(
    "Expected block {block} with single predecessor to have no phis or
    phis with a single operand, found multiple operands"
)]
pub struct ExpectedSingleOperandPhis {
    block: BlockId,
}
