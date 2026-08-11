//! CodeGraph —— 程式碼結構索引引擎。
//!
//! 規格見 `DESIGN.md`，架構見 `ARCHITECTURE.md`，實作順序見 `IMPLEMENTATION.md`。
//!
//! 這個 crate 是**唯一的商業邏輯所在**。`main.rs`（CLI）與之後的 MCP server
//! 都只是薄薄的介面層，不含邏輯——這樣兩個消費場景才會共用同一份行為。

pub mod cli;
pub mod error;
pub mod extract;
pub mod model;
pub mod project;
pub mod store;

pub use error::{CgError, NotIndexedReason, Result};
pub use extract::FileParse;
pub use model::{
    FileId, Kind, Provenance, RawRef, RawSymbol, Rel, Relation, Symbol, SymbolId, UnitId,
};
pub use project::Project;
pub use store::{Stats, Store};
