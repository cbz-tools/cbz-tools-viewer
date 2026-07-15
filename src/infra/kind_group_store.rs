use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use crate::domain::kind_group::{validate, CompiledKindRule, GroupDef, KindGroupConfig};

/// %LOCALAPPDATA%/cbz-viewer/kind_groups.toml
pub fn kind_groups_path() -> PathBuf {
    crate::session::app_base_dir().join("kind_groups.toml")
}

/// TOMLファイルの更新日時を取得（変更検出用）
pub fn last_modified() -> Option<SystemTime> {
    std::fs::metadata(kind_groups_path()).ok()?.modified().ok()
}

/// TOMLを読み込んでKindGroupConfigを返す
/// 失敗時はErrにエラーメッセージを返す（呼び出し元で直前の正常configを維持）
pub fn load() -> Result<KindGroupConfig, String> {
    let path = kind_groups_path();

    // ファイルが存在しない場合はテンプレートを生成してデフォルトを返す
    if !path.exists() {
        write_template(&path).map_err(|e| e.to_string())?;
        return Ok(KindGroupConfig::default());
    }

    let raw = std::fs::read_to_string(&path).map_err(|e| format!("読み込みエラー: {e}"))?;

    // BOM除去・改行コード正規化
    let content = raw.trim_start_matches('\u{FEFF}');
    let content = content.replace("\r\n", "\n").replace('\r', "\n");

    let toml_config: TomlConfig =
        toml::from_str(&content).map_err(|e| format!("構文エラー: {e}"))?;

    let config = build_config(toml_config).map_err(|e| format!("正規表現エラー: {e}"))?;

    // バリデーション（DAG検証含む）
    validate(&config).map_err(|e| format!("バリデーションエラー: {e}"))?;

    log::debug!(
        "[kind-group] loaded path={} rules={} overrides={}",
        path.display(),
        config.kind_rules.len(),
        config.overrides.len()
    );

    Ok(config)
}

/// overrides に複数件を一括追加・上書きしてTOMLに1回だけ書き戻す
pub fn set_overrides_bulk(entries: &[(String, String)]) -> Result<(), String> {
    let mut toml_config = load_raw().unwrap_or_default();
    for (path, group) in entries {
        toml_config.overrides.insert(path.clone(), group.clone());
    }
    save_raw(&toml_config)
}

/// overrides から複数件を一括削除してTOMLに1回だけ書き戻す
pub fn remove_overrides_bulk(paths: &[String]) -> Result<(), String> {
    let mut toml_config = load_raw().unwrap_or_default();
    for path in paths {
        toml_config.overrides.remove(path);
    }
    save_raw(&toml_config)
}

/// overrides の単一キーを old_path → new_path へ移行する。
pub fn rename_override(old_path: &str, new_path: &str) -> Result<bool, String> {
    if old_path == new_path {
        return Ok(false);
    }
    let mut toml_config = load_raw().unwrap_or_default();
    let Some(value) = toml_config.overrides.remove(old_path) else {
        return Ok(false);
    };
    toml_config.overrides.insert(new_path.to_owned(), value);
    save_raw(&toml_config)?;
    Ok(true)
}

// ---- 内部実装 ----

#[derive(Deserialize, Serialize, Default)]
struct TomlConfig {
    #[serde(default)]
    overrides: std::collections::HashMap<String, String>,
    #[serde(default)]
    kind_rules: Vec<TomlKindRule>,
    #[serde(default)]
    groups: std::collections::HashMap<String, TomlGroupDef>,
}

#[derive(Deserialize, Serialize)]
struct TomlKindRule {
    pattern: String,
    group: String,
}

#[derive(Deserialize, Serialize)]
struct TomlGroupDef {
    #[serde(default)]
    children: Vec<String>,
}

fn build_config(toml: TomlConfig) -> Result<KindGroupConfig, String> {
    let mut kind_rules = Vec::new();
    for rule in toml.kind_rules {
        let pattern = regex::Regex::new(&rule.pattern)
            .map_err(|e| format!("pattern '{}': {e}", rule.pattern))?;
        kind_rules.push(CompiledKindRule {
            pattern,
            group: rule.group,
        });
    }
    let groups = toml
        .groups
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                GroupDef {
                    children: v.children,
                },
            )
        })
        .collect();
    Ok(KindGroupConfig {
        overrides: toml.overrides,
        kind_rules,
        groups,
    })
}

fn load_raw() -> Result<TomlConfig, String> {
    let path = kind_groups_path();
    Ok(crate::infra::config_io::load_toml_or_default::<TomlConfig>(
        &path,
        "kind_group_overrides",
    ))
}

fn save_raw(config: &TomlConfig) -> Result<(), String> {
    let path = kind_groups_path();
    let content = toml::to_string(config).map_err(|e| e.to_string())?;
    crate::infra::config_io::atomic_write(&path, content.as_bytes()).map_err(|e| e.to_string())
}

fn write_template(path: &std::path::Path) -> std::io::Result<()> {
    crate::infra::config_io::atomic_write(
        path,
        include_str!("kind_groups_template.toml").as_bytes(),
    )
}
