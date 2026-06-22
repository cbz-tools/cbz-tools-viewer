/// ライブラリエントリのソートキー
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    #[default]
    NameNatural,
    Modified,
    Size,
    PageCount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}
