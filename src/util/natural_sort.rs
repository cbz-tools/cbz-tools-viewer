/// 自然順ソート（natord ラッパー）
pub fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    natord::compare(a, b)
}
