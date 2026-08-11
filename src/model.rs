//! 全專案共用的核心型別。

/// 符號的識別碼，對應 `monikers.id`。
///
/// 包成 newtype 讓編譯器區分不同的識別碼，執行期無額外成本。
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SymbolId(pub u32);

/// 檔案的識別碼，對應 `files.id`。
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FileId(pub u32);

/// 編譯單元的識別碼，對應 `units.id`。
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct UnitId(pub u32);

/// 符號種類。
///
/// 判別值會寫進資料庫，只能往後追加，不可修改既有數值。
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Kind {
    Function = 1,
    Method = 2,
    Class = 3,
    Struct = 4,
    Interface = 5,
    Trait = 6,
    Enum = 7,
    TypeAlias = 8,
    Const = 9,
    Module = 10,
    File = 11,
}

impl Kind {
    /// 從資料庫的判別值還原。
    ///
    /// 未知的值回 `None`，表示這份資料庫由較新的 schema 寫入。
    /// 呼叫端必須視為錯誤，不可略過。
    pub fn from_u8(v: u8) -> Option<Self> {
        use Kind::*;
        Some(match v {
            1 => Function,
            2 => Method,
            3 => Class,
            4 => Struct,
            5 => Interface,
            6 => Trait,
            7 => Enum,
            8 => TypeAlias,
            9 => Const,
            10 => Module,
            11 => File,
            _ => return None,
        })
    }

    /// 輸出用的標籤，同時是 moniker 的組成部分。
    ///
    /// 修改這些字串會使所有既有的 moniker 失效。
    pub fn as_str(self) -> &'static str {
        use Kind::*;
        match self {
            Function => "function",
            Method => "method",
            Class => "class",
            Struct => "struct",
            Interface => "interface",
            Trait => "trait",
            Enum => "enum",
            TypeAlias => "type",
            Const => "const",
            Module => "module",
            File => "file",
        }
    }
}

/// 邊的種類。判別值的相容性要求同 [`Kind`]。
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Rel {
    Calls = 1,
    Implements = 2,
    UsesType = 3,
    Extends = 4,
    References = 5,
    Contains = 6,
}

impl Rel {
    /// 從資料庫的判別值還原，未知的值回 `None`。
    pub fn from_u8(v: u8) -> Option<Self> {
        use Rel::*;
        Some(match v {
            1 => Calls,
            2 => Implements,
            3 => UsesType,
            4 => Extends,
            5 => References,
            6 => Contains,
            _ => return None,
        })
    }

    /// 輸出用的標籤。
    pub fn as_str(self) -> &'static str {
        use Rel::*;
        match self {
            Calls => "calls",
            Implements => "implements",
            UsesType => "uses_type",
            Extends => "extends",
            References => "references",
            Contains => "contains",
        }
    }
}

/// 邊的來源：靜態解析或啟發式合成。
///
/// 合成的邊精確度較低，查詢結果必須讓使用者分辨得出來。
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum Provenance {
    #[default]
    Static = 0,
    Heuristic = 1,
}

impl Provenance {
    /// 從資料庫的判別值還原，未知的值回 `None`。
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Provenance::Static),
            1 => Some(Provenance::Heuristic),
            _ => None,
        }
    }
}

/// 已寫入資料庫的符號。
#[derive(Clone, Debug)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: Kind,
    pub file: FileId,
    /// 起始行，1 起算。
    pub start_line: u32,
    /// 結束行，1 起算。
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
}

/// 已寫入資料庫的邊。
#[derive(Clone, Debug)]
pub struct Relation {
    pub src: SymbolId,
    pub dst: SymbolId,
    pub rel: Rel,
    /// 引用發生的位置。合成的邊沒有位置。
    pub file: Option<FileId>,
    pub line: Option<u32>,
    pub provenance: Provenance,
    /// 合成器名稱與註冊位置，僅 [`Provenance::Heuristic`] 的邊使用。
    pub meta: Option<String>,
}

/// 抽取階段產生的符號。
///
/// 抽取層不存取資料庫，因此還沒有識別碼；與已寫入的符號之間靠
/// `moniker` 對應。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSymbol {
    /// 穩定識別碼，格式為 `路徑:kind:name:起始行`。
    pub moniker: String,
    /// 未限定的名字，例如 `open`。
    pub name: String,
    /// 含容器的名字，例如 `Store::open`。
    pub qualified: String,
    pub kind: Kind,
    /// 起始行，1 起算。
    pub start_line: u32,
    /// 結束行，1 起算。
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
}

/// 抽取階段產生、尚未解析成 [`Relation`] 的引用。
///
/// 抽取層不做跨檔推論，只記錄引用的名字，解析由 resolve 層負責。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRef {
    /// 發出引用的符號的 moniker。
    pub from: String,
    /// 引用的原文，例如 `utils.greet`。
    pub name: String,
    pub rel: Rel,
    pub line: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrips_through_u8() {
        for k in [
            Kind::Function,
            Kind::Method,
            Kind::Class,
            Kind::Struct,
            Kind::Interface,
            Kind::Trait,
            Kind::Enum,
            Kind::TypeAlias,
            Kind::Const,
            Kind::Module,
            Kind::File,
        ] {
            assert_eq!(Kind::from_u8(k as u8), Some(k));
        }
    }

    #[test]
    fn rel_roundtrips_through_u8() {
        for r in [
            Rel::Calls,
            Rel::Implements,
            Rel::UsesType,
            Rel::Extends,
            Rel::References,
            Rel::Contains,
        ] {
            assert_eq!(Rel::from_u8(r as u8), Some(r));
        }
    }

    #[test]
    fn unknown_discriminants_are_rejected_not_guessed() {
        assert_eq!(Kind::from_u8(0), None);
        assert_eq!(Kind::from_u8(99), None);
        assert_eq!(Rel::from_u8(0), None);
        assert_eq!(Rel::from_u8(7), None);
        assert_eq!(Provenance::from_u8(2), None);
    }

    #[test]
    fn provenance_roundtrips_and_defaults_to_static() {
        assert_eq!(Provenance::from_u8(0), Some(Provenance::Static));
        assert_eq!(Provenance::from_u8(1), Some(Provenance::Heuristic));
        assert_eq!(Provenance::default(), Provenance::Static);
        assert_eq!(Provenance::Static as u8, 0);
        assert_eq!(Provenance::Heuristic as u8, 1);
    }

    #[test]
    fn kind_labels_are_pinned() {
        let pairs = [
            (Kind::Function, "function"),
            (Kind::Method, "method"),
            (Kind::Class, "class"),
            (Kind::Struct, "struct"),
            (Kind::Interface, "interface"),
            (Kind::Trait, "trait"),
            (Kind::Enum, "enum"),
            (Kind::TypeAlias, "type"),
            (Kind::Const, "const"),
            (Kind::Module, "module"),
            (Kind::File, "file"),
        ];
        for (k, s) in pairs {
            assert_eq!(k.as_str(), s);
        }

        // 標籤重複會使兩種 kind 產生相同的 moniker。
        let mut labels: Vec<&str> = pairs.iter().map(|(_, s)| *s).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "kind 標籤有重複");
    }

    #[test]
    fn rel_labels_are_pinned_and_distinct() {
        let pairs = [
            (Rel::Calls, "calls"),
            (Rel::Implements, "implements"),
            (Rel::UsesType, "uses_type"),
            (Rel::Extends, "extends"),
            (Rel::References, "references"),
            (Rel::Contains, "contains"),
        ];
        for (r, s) in pairs {
            assert_eq!(r.as_str(), s);
        }

        let mut labels: Vec<&str> = pairs.iter().map(|(_, s)| *s).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "rel 標籤有重複");
    }

    #[test]
    fn ids_are_distinct_types_with_transparent_values() {
        let s = SymbolId(7);
        let f = FileId(7);
        let u = UnitId(7);
        assert_eq!(s.0, 7);
        assert_eq!(f.0, 7);
        assert_eq!(u.0, 7);
        assert_eq!(s, SymbolId(7));
        assert_ne!(s, SymbolId(8));
        assert!(SymbolId(1) < SymbolId(2));
    }

    #[test]
    fn structs_carry_what_the_schema_needs() {
        let sym = Symbol {
            id: SymbolId(1),
            name: "open".into(),
            kind: Kind::Function,
            file: FileId(2),
            start_line: 10,
            end_line: 20,
            signature: Some("fn open(p: &Path) -> Result<Store>".into()),
            docstring: None,
        };
        assert_eq!(sym.end_line - sym.start_line, 10);

        let rel = Relation {
            src: SymbolId(1),
            dst: SymbolId(3),
            rel: Rel::Calls,
            file: Some(FileId(2)),
            line: Some(12),
            provenance: Provenance::Static,
            meta: None,
        };
        assert_eq!(rel.provenance, Provenance::Static);

        let synth = Relation {
            src: SymbolId(1),
            dst: SymbolId(4),
            rel: Rel::Calls,
            file: None,
            line: None,
            provenance: Provenance::Heuristic,
            meta: Some("callback@src/a.rs:30".into()),
        };
        assert!(synth.meta.is_some());

        let raw = RawRef {
            from: "src/a.rs:function:caller:1".into(),
            name: "utils.greet".into(),
            rel: Rel::Calls,
            line: 12,
        };
        assert_eq!(raw.name.rsplit('.').next(), Some("greet"));

        let raw_sym = RawSymbol {
            moniker: "src/a.rs:function:caller:1".into(),
            name: "caller".into(),
            qualified: "caller".into(),
            kind: Kind::Function,
            start_line: 1,
            end_line: 5,
            signature: Some("fn caller()".into()),
            docstring: None,
        };
        assert_eq!(raw.from, raw_sym.moniker);
    }
}
