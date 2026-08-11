//! CodeGraph：程式碼結構索引引擎。
//!
//! 這個 crate 包含全部的實作。CLI 與後續的服務介面都只負責解析輸入、
//! 呼叫這裡的函數並輸出結果，兩者共用同一份行為。

pub mod cli;
pub mod error;
pub mod explore;
pub mod extract;
pub mod graph;
pub mod indexer;
pub mod model;
pub mod project;
pub mod resolve;
pub mod store;

pub use error::{CgError, NotIndexedReason, Result};
pub use explore::{Exploration, budget::Budget};
pub use extract::FileParse;
pub use indexer::IndexReport;
pub use model::{
    FileId, Kind, Provenance, RawRef, RawSymbol, Rel, Relation, Symbol, SymbolId, UnitId,
};
pub use project::Project;
pub use store::{Stats, Store};
