//! 輸出預算。
//!
//! 專案越大，一個問題需要看的程式碼越多，預算跟著放寬。

/// 一次查詢可以用掉的額度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// 單次查詢的輸出上限，以字元計。
    pub max_chars: usize,
    /// 同一個檔案在單次查詢中的輸出上限。
    pub max_chars_per_file: usize,
}

/// 分級表，依已索引的檔案數查表。
///
/// 每一列是（檔案數上限, 額度）。最後一列的檔案數上限為
/// [`usize::MAX`]，涵蓋所有更大的專案。
const TIERS: [(usize, Budget); 5] = [
    (
        500,
        Budget {
            max_chars: 18_000,
            max_chars_per_file: 3_800,
        },
    ),
    (
        5_000,
        Budget {
            max_chars: 28_000,
            max_chars_per_file: 6_500,
        },
    ),
    (
        15_000,
        Budget {
            max_chars: 35_000,
            max_chars_per_file: 7_000,
        },
    ),
    (
        25_000,
        Budget {
            max_chars: 38_000,
            max_chars_per_file: 7_000,
        },
    ),
    (
        usize::MAX,
        Budget {
            max_chars: 38_000,
            max_chars_per_file: 7_000,
        },
    ),
];

/// 依已索引的檔案數取得預算。
pub fn for_file_count(files: usize) -> Budget {
    for (limit, budget) in TIERS {
        if files < limit {
            return budget;
        }
    }
    TIERS[TIERS.len() - 1].1
}

/// 一段輸出至少要有這麼多字元才值得放進來。
///
/// 額度剩下的空間放不下幾行程式碼時，寧可整段略過並回報數量，也不要
/// 給出看不懂的殘片。
pub const MIN_USEFUL_CHARS: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tier_is_selected_by_file_count() {
        assert_eq!(for_file_count(0).max_chars, 18_000);
        assert_eq!(for_file_count(499).max_chars, 18_000);
        assert_eq!(for_file_count(500).max_chars, 28_000);
        assert_eq!(for_file_count(4_999).max_chars, 28_000);
        assert_eq!(for_file_count(5_000).max_chars, 35_000);
        assert_eq!(for_file_count(15_000).max_chars, 38_000);
        assert_eq!(for_file_count(usize::MAX).max_chars, 38_000);
    }

    /// 大專案的每一項額度都不得小於小專案。
    ///
    /// 單檔上限特別容易被寫反：把中型專案設得比小型專案還小，遇到超大
    /// 檔案時一次查詢只回得了整份檔案的極小一部分，呼叫端只好自己去
    /// 開檔案，整個工具就失去意義。
    #[test]
    fn every_limit_is_monotonic_across_tiers() {
        let mut previous: Option<Budget> = None;

        for (limit, budget) in TIERS {
            if let Some(prev) = previous {
                assert!(
                    budget.max_chars >= prev.max_chars,
                    "檔案數上限 {limit} 的輸出上限比前一級小"
                );
                assert!(
                    budget.max_chars_per_file >= prev.max_chars_per_file,
                    "檔案數上限 {limit} 的單檔上限比前一級小"
                );
            }
            previous = Some(budget);
        }
    }

    #[test]
    fn tiers_are_listed_in_ascending_order() {
        let mut previous = 0;
        for (limit, _) in TIERS {
            assert!(limit > previous, "分級表的檔案數上限沒有遞增");
            previous = limit;
        }
    }

    /// 單檔上限必須小於總輸出上限，否則一個檔案就能吃光整次查詢。
    #[test]
    fn a_single_file_can_never_consume_the_whole_budget() {
        for (_, budget) in TIERS {
            assert!(budget.max_chars_per_file < budget.max_chars);
        }
    }

    #[test]
    fn a_useful_fragment_threshold_is_smaller_than_any_per_file_limit() {
        for (_, budget) in TIERS {
            assert!(MIN_USEFUL_CHARS < budget.max_chars_per_file);
        }
    }
}
