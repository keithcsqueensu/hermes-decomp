mod analyzer;
mod builder;
mod ctor_this;
mod utils;

use crate::ir::Statement;
use analyzer::ClassAnalyzer;
pub use ctor_this::restore_constructor_this;

pub fn detect_class_patterns(
    stmts: Vec<Statement>,
    file: &crate::BytecodeFile,
    format: &crate::BytecodeFormat,
    options: &crate::DecompileOptionsV2,
    closure_ctx: Option<&crate::ClosureContext>,
) -> Vec<Statement> {
    let mut analyzer = ClassAnalyzer::new(file, format, options, closure_ctx);
    analyzer.analyze(stmts)
}
