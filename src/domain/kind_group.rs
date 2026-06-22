use std::collections::{HashMap, HashSet};

use regex::Regex;

#[derive(Default)]
pub struct KindGroupConfig {
    /// キー: normalize_path_for_override() 結果
    pub overrides: HashMap<String, String>,
    /// 起動時・TOMLリロード時に一度だけコンパイル済み
    pub kind_rules: Vec<CompiledKindRule>,
    pub groups: HashMap<String, GroupDef>,
}

pub struct CompiledKindRule {
    pub pattern: Regex,
    pub group: String,
}

pub struct GroupDef {
    pub children: Vec<String>,
}

impl KindGroupConfig {
    /// パスからグループ名を解決する
    /// 優先度1: overrides（正規化パス完全一致）
    /// 優先度2: kind_rules（上から順に評価・ヒットしたら終了）
    /// 優先度3: Kind値そのもの
    /// それ以外: None（未分類）
    pub fn resolve(&self, normalized_path: &str, kind: Option<&str>) -> Option<String> {
        // 優先度1: overrides（正規化パス完全一致）
        if let Some(group) = self.overrides.get(normalized_path) {
            return Some(group.clone());
        }
        // 優先度2: kind_rules（パターンマッチ・TOMLで定義）
        if let Some(kind_str) = kind {
            for rule in &self.kind_rules {
                if rule.pattern.is_match(kind_str) {
                    return Some(rule.group.clone());
                }
            }
            // 優先度3: Kind値そのものがグループ名（自動タグ付けの本体）
            return Some(kind_str.to_string());
        }
        // Kind値なし → 未分類（None）
        None
    }
}

/// TOMLロード時バリデーション
/// 前提: この関数が Ok を返したものだけ compute_parent_counts() に渡す
pub fn validate(config: &KindGroupConfig) -> Result<(), String> {
    // 1. DAG検証（循環参照検出）
    detect_cycle(config)?;

    // 2. known_groups構築（rules/overrides/groupsキー全て）
    let known_groups: HashSet<&str> = config
        .kind_rules
        .iter()
        .map(|r| r.group.as_str())
        .chain(config.overrides.values().map(|v| v.as_str()))
        .chain(config.groups.keys().map(|k| k.as_str()))
        .collect();

    // 3. child妥当性検証・重複child検出
    for (parent, def) in &config.groups {
        let mut seen = HashSet::new();
        for child in &def.children {
            if !known_groups.contains(child.as_str()) {
                return Err(format!("未定義のchildグループ: {child} (親: {parent})"));
            }
            if !seen.insert(child.as_str()) {
                return Err(format!("重複したchild: {child} (親: {parent})"));
            }
        }
    }
    Ok(())
}

fn detect_cycle(config: &KindGroupConfig) -> Result<(), String> {
    // DFSで循環参照を検出
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: HashSet<&str> = HashSet::new();

    for key in config.groups.keys() {
        if !visited.contains(key.as_str()) {
            dfs_cycle(key, &config.groups, &mut visited, &mut stack)?;
        }
    }
    Ok(())
}

fn dfs_cycle<'a>(
    node: &'a str,
    groups: &'a HashMap<String, GroupDef>,
    visited: &mut HashSet<&'a str>,
    stack: &mut HashSet<&'a str>,
) -> Result<(), String> {
    visited.insert(node);
    stack.insert(node);
    if let Some(def) = groups.get(node) {
        for child in &def.children {
            if stack.contains(child.as_str()) {
                return Err(format!("循環参照を検出: {child}"));
            }
            if !visited.contains(child.as_str()) {
                dfs_cycle(child, groups, visited, stack)?;
            }
        }
    }
    stack.remove(node);
    Ok(())
}
