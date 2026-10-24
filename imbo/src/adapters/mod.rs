//! One adapter per tool: discover sessions and put messages into them.

pub mod claude;
pub mod codex;

/// `IMBO_FAULT_INJECT=claude,codex` makes inserts fail (tests of the fallbacks).
pub fn fault(tool: &str) -> bool {
    std::env::var("IMBO_FAULT_INJECT")
        .map(|v| v.split(',').any(|t| t.trim() == tool))
        .unwrap_or(false)
}
