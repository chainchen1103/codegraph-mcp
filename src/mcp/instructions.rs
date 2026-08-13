//! `initialize` 回傳的指引。
//!
//! 這是 agent 指引的**唯一來源**。工具描述只講參數，該怎麼用寫在這裡，
//! 兩邊都寫會互相牴觸，而且改了一邊另一邊就過期。

/// 給 agent 的使用指引。
pub const INSTRUCTIONS: &str = "\
CodeGraph 是這個工作區的程式碼結構索引。要理解或定位程式碼時，先呼叫這裡的工具，
再考慮讀檔或搜尋。

explore 是主入口，和 Read 同級：給它一個問題，或一串符號名／限定名／檔案路徑，
它回傳相關符號的逐字帶行號原始碼（依檔案分組，可直接據以編輯），以及被指名的符號
之間的呼叫路徑。一次呼叫通常就問完一件事。

node 是深挖用：指名單一符號，回傳完整 body（不裁切）與它兩側的呼叫關係。名字有
多個定義時全部回傳。

status 回報索引規模與新鮮度，不佔輸出額度。

要點：
- 工具永遠在，即使當前目錄沒有索引。沒有索引時回應會說明下一步，那不是錯誤。
- projectPath 可以指向工作區裡任何一個已索引的目錄，monorepo 只有子專案有索引是
  常見情況。
- 工具不會自行建立索引，那是使用者的決定。
- 回應裡出現「稍早已送出」的檔案清單，表示那些內容這次對話已經給過而且沒有變，
  是指標不是缺漏，不需要再讀一次。
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instructions_cover_all_three_tools() {
        for tool in ["explore", "node", "status"] {
            assert!(INSTRUCTIONS.contains(tool), "少了 {tool}");
        }
    }

    /// 沒有索引不是錯誤，這件事一定要講，否則 agent 會停用工具。
    #[test]
    fn the_instructions_say_a_missing_index_is_not_an_error() {
        assert!(INSTRUCTIONS.contains("那不是錯誤"));
        assert!(INSTRUCTIONS.contains("projectPath"));
    }
}
