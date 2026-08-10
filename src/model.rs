//! 核心型別。全專案共用，其他模組不得自行定義平行的型別。
//!
//! 見 ARCHITECTURE.md §3。

/// 符號的整數識別碼。等同 `monikers.id`。
///
/// 用 newtype 包 `u32` 而不是裸 `u32`，是為了讓編譯器擋掉
/// 「把 FileId 傳給吃 SymbolId 的函數」這類錯誤。執行期零成本。
///
/// 跨 `base.db` / `pr.db` 必須對齊，見 DESIGN.md §3.2。
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SymbolId(pub u32);

/// 檔案的整數識別碼。等同 `files.id`。
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FileId(pub u32);

/// 編譯單元的整數識別碼。等同 `units.id`。
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct UnitId(pub u32);

/// 符號種類。DB 存 integer，Rust 端有窮盡比對。
///
/// 數值一旦寫進使用者的 DB 就不能改，只能往後追加。
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
    /// 從 DB 讀回來的 integer 還原。未知值回 `None`——
    /// 代表這份 DB 是更新版本的 schema 寫的，呼叫端要當成錯誤處理，
    /// 不可以靜默略過。
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

    /// 輸出用的短標籤。也是 moniker 的組成部分之一，
    /// 因此**改動它會讓所有既有識別碼失效**（見 DESIGN.md §8.3）。
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

/// 邊的種類。
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

/// 邊的來源。靜態解析出來的，或啟發式合成的。
///
/// 合成邊必須能被使用者一眼認出來——`explore` 的 Flow 區塊會把
/// 合成跳點與註冊位置攤開顯示（DESIGN.md §7.3）。
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum Provenance {
    #[default]
    Static = 0,
    Heuristic = 1,
}

impl Provenance {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Provenance::Static),
            1 => Some(Provenance::Heuristic),
            _ => None,
        }
    }
}

/// 一個符號。
///
/// `moniker` 只在寫入期需要（用來 intern 成 id），讀取期一律用 `SymbolId`——
/// 字串在熱路徑上就是 allocation 與比較成本。
#[derive(Clone, Debug)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: Kind,
    pub file: FileId,
    /// 1 起算。tree-sitter 的 row 是 0 起算，轉換在 extract 層做。
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
}

/// 一條邊。
#[derive(Clone, Debug)]
pub struct Relation {
    pub src: SymbolId,
    pub dst: SymbolId,
    pub rel: Rel,
    /// 呼叫點所在的檔案與行。合成邊可能沒有座標。
    pub file: Option<FileId>,
    pub line: Option<u32>,
    pub provenance: Provenance,
    /// 僅 heuristic 邊使用：合成器名稱與註冊位置。
    pub meta: Option<String>,
}

/// 抽取階段產生、尚未解析成 `Relation` 的引用。
///
/// 抽取層不做跨檔推論，只忠實記下「這裡引用了叫這個名字的東西」，
/// 解析交給 resolve 層（ARCHITECTURE.md §6）。
#[derive(Clone, Debug)]
pub struct RawRef {
    pub from: SymbolId,
    /// 原文，例如 `"utils.greet"`。
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

    /// `Kind::as_str` 是 moniker 的組成部分之一，改動它會讓所有既有
    /// 識別碼失效（DESIGN.md §8.3）。這個測試把字面值釘死，
    /// 讓「不小心改字串」變成一個測試失敗而不是靜默的索引損壞。
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
        // 標籤必須互異，否則兩種 kind 會產生相同的 moniker。
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

    /// newtype 的意義就在這裡：SymbolId 與 FileId 不可互換。
    /// 這個測試主要是文件性質——真正的保障是編譯期，
    /// `takes_symbol(FileId(1))` 根本編譯不過。
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

        // 合成邊沒有座標，且一定要帶 meta 說明來源（DESIGN.md §7.3）。
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
            from: SymbolId(1),
            name: "utils.greet".into(),
            rel: Rel::Calls,
            line: 12,
        };
        assert_eq!(raw.name.rsplit('.').next(), Some("greet"));
    }
}
