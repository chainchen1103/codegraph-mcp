//! 兩個符號之間的呼叫路徑。

use std::collections::{HashMap, VecDeque};

use rusqlite::Connection;

use crate::error::Result;
use crate::model::{Kind, Provenance, Rel, SymbolId};

/// 路徑上未被指名的中間節點上限。
const MAX_BRIDGES: usize = 3;

/// 可以充當中間節點的最大出邊數。
const HUB_FANOUT: usize = 32;

/// 路徑上的一跳。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub id: SymbolId,
    pub qualified: String,
    pub kind: Kind,
    /// 這個符號自己所在的檔案。
    pub file: String,
    /// 呼叫下一跳的行號。最後一跳沒有下一跳，為 `None`。
    pub line: Option<u32>,
    /// 走到這一跳所用的那條邊的來源。第一跳固定為 [`Provenance::Static`]。
    pub provenance: Provenance,
}

/// 一條由呼叫邊串起來的路徑，至少兩跳。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub hops: Vec<Hop>,
}

impl Path {
    /// 兩端之外的中間節點數。
    pub fn bridges(&self) -> usize {
        self.hops.len().saturating_sub(2)
    }

    /// 路徑上是否有合成的邊。
    pub fn has_heuristic(&self) -> bool {
        self.hops
            .iter()
            .any(|h| h.provenance == Provenance::Heuristic)
    }
}

/// 走到某個節點的那條邊。
#[derive(Clone, Copy)]
struct Arrival {
    from: SymbolId,
    /// 呼叫點的行號，`None` 表示這條邊沒有位置。
    line: Option<u32>,
    provenance: Provenance,
}

/// 從 `from` 沿呼叫邊走到 `to` 的最短路徑。
///
/// 方向固定：`from` 呼叫 `to`。兩端相同或走不到時回 `None`。
pub fn shortest(conn: &Connection, from: SymbolId, to: SymbolId) -> Result<Option<Path>> {
    if from == to {
        return Ok(None);
    }

    let mut arrivals: HashMap<SymbolId, Arrival> = HashMap::new();
    let mut depths: HashMap<SymbolId, usize> = HashMap::from([(from, 0)]);
    let mut queue = VecDeque::from([from]);

    while let Some(current) = queue.pop_front() {
        let depth = depths[&current];
        // 中間節點的數量 = 邊數 - 1。
        if depth > MAX_BRIDGES {
            continue;
        }

        let edges = outgoing(conn, current)?;
        // 端點不受出度限制，只有中間節點會被擋下。
        if current != from && edges.len() > HUB_FANOUT {
            continue;
        }

        for (next, line, provenance) in edges {
            if depths.contains_key(&next) {
                continue;
            }
            depths.insert(next, depth + 1);
            arrivals.insert(
                next,
                Arrival {
                    from: current,
                    line,
                    provenance,
                },
            );
            if next == to {
                return hydrate(conn, trace(&arrivals, from, to)).map(Some);
            }
            queue.push_back(next);
        }
    }

    Ok(None)
}

/// 一個符號的呼叫邊，依呼叫點排序讓結果穩定。
fn outgoing(conn: &Connection, src: SymbolId) -> Result<Vec<(SymbolId, Option<u32>, Provenance)>> {
    let mut stmt = conn.prepare(
        "SELECT dst, line, provenance FROM relations
         WHERE src = ?1 AND rel = ?2
         ORDER BY line, dst",
    )?;
    let rows = stmt.query_map(rusqlite::params![src.0, Rel::Calls as u8], |r| {
        let line: i64 = r.get(1)?;
        let provenance: u8 = r.get(2)?;
        Ok((
            SymbolId(r.get(0)?),
            // -1 代表這條邊沒有位置，例如合成的邊。
            (line >= 0).then_some(line as u32),
            Provenance::from_u8(provenance).unwrap_or(Provenance::Static),
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 沿著抵達記錄從終點回推。
///
/// 回傳依序排列的節點，以及節點之間那些邊的位置與來源。邊比節點少一個。
fn trace(
    arrivals: &HashMap<SymbolId, Arrival>,
    from: SymbolId,
    to: SymbolId,
) -> (Vec<SymbolId>, Vec<(Option<u32>, Provenance)>) {
    let mut nodes = vec![to];
    let mut edges = Vec::new();
    let mut node = to;

    while node != from {
        let arrival = arrivals[&node];
        edges.push((arrival.line, arrival.provenance));
        nodes.push(arrival.from);
        node = arrival.from;
    }

    nodes.reverse();
    edges.reverse();
    (nodes, edges)
}

/// 把節點順序補上符號資料。
///
/// 一跳帶的行號是它呼叫下一跳的位置，最後一跳沒有下一跳因此為 `None`；
/// 來源則取自走進這一跳的那條邊，起點沒有來邊，固定為靜態。
fn hydrate(
    conn: &Connection,
    (nodes, edges): (Vec<SymbolId>, Vec<(Option<u32>, Provenance)>),
) -> Result<Path> {
    let mut hops = Vec::with_capacity(nodes.len());

    for (index, id) in nodes.iter().enumerate() {
        let (qualified, kind, file) = describe(conn, *id)?;
        hops.push(Hop {
            id: *id,
            qualified,
            kind,
            file,
            line: edges.get(index).and_then(|(line, _)| *line),
            provenance: index
                .checked_sub(1)
                .map_or(Provenance::Static, |i| edges[i].1),
        });
    }

    Ok(Path { hops })
}

/// 取出一個符號的顯示資料。
fn describe(conn: &Connection, id: SymbolId) -> Result<(String, Kind, String)> {
    let row = conn.query_row(
        "SELECT s.qualified, s.kind, f.path
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.id = ?1",
        [id.0],
        |r| {
            let kind: u8 = r.get(1)?;
            Ok((
                r.get::<_, String>(0)?,
                Kind::from_u8(kind).unwrap_or(Kind::Function),
                r.get::<_, String>(2)?,
            ))
        },
    )?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::testing::resolved;

    fn id_of(store: &Store, qualified: &str) -> SymbolId {
        SymbolId(
            store
                .conn()
                .query_row(
                    "SELECT id FROM symbols WHERE qualified = ?1",
                    [qualified],
                    |r| r.get(0),
                )
                .unwrap(),
        )
    }

    fn find(store: &Store, from: &str, to: &str) -> Option<Path> {
        shortest(store.conn(), id_of(store, from), id_of(store, to)).unwrap()
    }

    fn names(path: &Path) -> Vec<&str> {
        path.hops.iter().map(|h| h.qualified.as_str()).collect()
    }

    /// 依序呼叫下去的一條鏈：`links[0]` 呼叫 `links[1]`，以此類推。
    fn chain(links: &[&str]) -> Store {
        let mut source = String::new();
        for (i, name) in links.iter().enumerate() {
            match links.get(i + 1) {
                Some(next) => source.push_str(&format!("pub fn {name}() {{\n    {next}();\n}}\n")),
                None => source.push_str(&format!("pub fn {name}() {{}}\n")),
            }
        }
        resolved(&[("src/a.rs", &source)])
    }

    #[test]
    fn a_direct_call_is_a_two_hop_path() {
        let store = chain(&["caller", "callee"]);
        let path = find(&store, "caller", "callee").unwrap();

        assert_eq!(names(&path), vec!["caller", "callee"]);
        assert_eq!(path.bridges(), 0);
    }

    /// 每一跳帶的是它呼叫下一跳的位置，終點沒有下一跳。
    #[test]
    fn each_hop_carries_the_call_site_of_the_next_one() {
        let store = chain(&["a", "b", "c"]);
        let path = find(&store, "a", "c").unwrap();

        assert_eq!(names(&path), vec!["a", "b", "c"]);
        assert_eq!(path.hops[0].line, Some(2));
        assert_eq!(path.hops[1].line, Some(5));
        assert_eq!(path.hops[2].line, None);
        assert!(path.hops.iter().all(|h| h.file == "src/a.rs"));
        assert_eq!(path.hops[0].kind, Kind::Function);
    }

    #[test]
    fn two_symbols_that_never_reach_each_other_have_no_path() {
        let store = resolved(&[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")]);
        assert!(find(&store, "one", "two").is_none());
    }

    /// 呼叫關係有方向，反著問不會拿到同一條路徑。
    #[test]
    fn the_path_is_directed() {
        let store = chain(&["caller", "callee"]);
        assert!(find(&store, "caller", "callee").is_some());
        assert!(find(&store, "callee", "caller").is_none());
    }

    #[test]
    fn a_symbol_has_no_path_to_itself() {
        let store = chain(&["a", "b"]);
        let a = id_of(&store, "a");
        assert!(shortest(store.conn(), a, a).unwrap().is_none());
    }

    /// 有多條路可走時取最短的那條。
    #[test]
    fn the_shortest_route_wins() {
        let store = resolved(&[(
            "src/a.rs",
            "pub fn target() {}\n\
             pub fn middle() {\n    target();\n}\n\
             pub fn start() {\n    middle();\n    target();\n}\n",
        )]);

        let path = find(&store, "start", "target").unwrap();
        assert_eq!(names(&path), vec!["start", "target"]);
    }

    /// 隔太多層的兩個符號之間沒有值得回報的路徑。
    #[test]
    fn a_path_longer_than_the_bridge_limit_is_refused() {
        let links: Vec<String> = (0..7).map(|i| format!("f{i}")).collect();
        let borrowed: Vec<&str> = links.iter().map(String::as_str).collect();
        let store = chain(&borrowed);

        let longest = find(&store, "f0", &format!("f{}", MAX_BRIDGES + 1)).unwrap();
        assert_eq!(longest.bridges(), MAX_BRIDGES);

        assert!(
            find(&store, "f0", &format!("f{}", MAX_BRIDGES + 2)).is_none(),
            "超過橋接上限的路徑不該回報"
        );
    }

    /// 出邊極多的符號不得當橋接，否則任意兩個符號都會被它連起來。
    #[test]
    fn a_hub_cannot_serve_as_a_bridge() {
        let fanout = HUB_FANOUT + 8;
        let mut source = String::from("pub fn entry() {\n    hub();\n}\npub fn hub() {\n");
        for i in 0..fanout {
            source.push_str(&format!("    leaf{i}();\n"));
        }
        source.push_str("}\n");
        for i in 0..fanout {
            source.push_str(&format!("pub fn leaf{i}() {{}}\n"));
        }
        let store = resolved(&[("src/a.rs", &source)]);

        assert!(
            find(&store, "entry", "leaf0").is_none(),
            "god function 讓路徑穿了過去"
        );
        assert!(
            find(&store, "hub", "leaf0").is_some(),
            "hub 當端點時仍要走得通"
        );
    }

    /// 合成的邊要能被辨認出來，呼叫端才判斷得了這一跳可不可信。
    #[test]
    fn a_synthesised_hop_is_marked_as_heuristic() {
        let store = chain(&["a", "b"]);
        let (a, b) = (id_of(&store, "a"), id_of(&store, "b"));
        store
            .conn()
            .execute(
                "UPDATE relations SET provenance = ?3 WHERE src = ?1 AND dst = ?2",
                rusqlite::params![a.0, b.0, Provenance::Heuristic as u8],
            )
            .unwrap();

        let path = find(&store, "a", "b").unwrap();
        assert_eq!(path.hops[0].provenance, Provenance::Static, "起點沒有來邊");
        assert_eq!(path.hops[1].provenance, Provenance::Heuristic);
        assert!(path.has_heuristic());
    }

    #[test]
    fn a_fully_static_path_is_not_flagged() {
        let store = chain(&["a", "b", "c"]);
        assert!(!find(&store, "a", "c").unwrap().has_heuristic());
    }

    /// 合成的邊沒有呼叫點，行號欄位要留空而不是印出 -1。
    #[test]
    fn an_edge_without_a_call_site_has_no_line() {
        let store = chain(&["a", "b"]);
        let (a, b) = (id_of(&store, "a"), id_of(&store, "b"));
        store
            .conn()
            .execute(
                "UPDATE relations SET line = -1 WHERE src = ?1 AND dst = ?2",
                rusqlite::params![a.0, b.0],
            )
            .unwrap();

        let path = find(&store, "a", "b").unwrap();
        assert_eq!(path.hops[0].line, None);
    }
}
