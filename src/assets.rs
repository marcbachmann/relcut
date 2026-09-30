use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub file: PathBuf,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
}

impl Asset {
    pub fn new(file: impl Into<PathBuf>) -> Asset {
        let file = file.into();
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let content_type = content_type(&name).to_string();
        Asset {
            file,
            name,
            label: None,
            content_type,
        }
    }
}

pub fn content_type(name: &str) -> &'static str {
    let name = name.to_lowercase();
    let ext = |e: &str| name.ends_with(e);
    if ext(".cdx.json") {
        "application/vnd.cyclonedx+json"
    } else if ext(".spdx.json") {
        "application/spdx+json"
    } else if ext(".tgz") || ext(".tar.gz") || ext(".gz") {
        "application/gzip"
    } else if ext(".zip") {
        "application/zip"
    } else if ext(".tar") {
        "application/x-tar"
    } else if ext(".json") {
        "application/json"
    } else if ext(".txt") || ext(".md") || name.ends_with("sums") {
        "text/plain"
    } else if ext(".html") {
        "text/html"
    } else if ext(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}

// Each value is a path, a list of them, or JSON with metadata:
// [{"file": "dist/x", "name": "x-linux", "label": "Linux", "content_type": "…"}]
pub fn parse(values: &[String]) -> Result<Vec<Asset>, String> {
    let mut assets = Vec::new();
    for value in values {
        let value = value.trim();
        if value.starts_with('[') || value.starts_with('{') {
            let json: Value =
                serde_json::from_str(value).map_err(|e| format!("github-assets: {e}"))?;
            let entries = match json {
                Value::Array(entries) => entries,
                entry => vec![entry],
            };
            for entry in entries {
                assets.push(from_json(&entry)?);
            }
        } else {
            assets.extend(
                value
                    .split(['\n', '\r', ','])
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(Asset::new),
            );
        }
    }
    Ok(assets)
}

fn from_json(entry: &Value) -> Result<Asset, String> {
    let text = |key: &str| entry[key].as_str().map(str::to_string);
    let file = text("file").ok_or(format!("github-assets: {entry} has no \"file\""))?;
    let mut asset = Asset::new(file);
    if let Some(name) = text("name") {
        asset.content_type = content_type(&name).to_string();
        asset.name = name;
    }
    asset.label = text("label");
    if let Some(ct) = text("content_type") {
        asset.content_type = ct;
    }
    if let Some(unknown) = entry.as_object().and_then(|o| {
        o.keys()
            .find(|k| !["file", "name", "label", "content_type"].contains(&k.as_str()))
    }) {
        return Err(format!(
            "github-assets: unknown key \"{unknown}\", entries take file, name, label and content_type"
        ));
    }
    Ok(asset)
}

// Before the tag: every file is there, and no two share a name.
pub fn check(assets: &[Asset]) -> Result<(), String> {
    let mut names = std::collections::BTreeSet::new();
    for asset in assets {
        if !asset.file.is_file() {
            return Err(format!(
                "github-assets: {} is not a file",
                asset.file.display()
            ));
        }
        if !names.insert(asset.name.as_str()) {
            return Err(format!("github-assets: two files are named {}", asset.name));
        }
    }
    Ok(())
}

// relcut-x86_64-linux · 2.1 MB · application/octet-stream · dist/relcut-linux
pub fn describe(asset: &Asset) -> Result<String, String> {
    let bytes = std::fs::metadata(&asset.file)
        .map_err(|e| format!("{}: {e}", asset.file.display()))?
        .len();
    let label = asset
        .label
        .as_ref()
        .map_or(String::new(), |l| format!(" · {l}"));
    let path = crate::log::short(&asset.file.display().to_string());
    let from = if path == asset.name {
        String::new()
    } else {
        format!(" · {path}")
    };
    Ok(format!(
        "{}{label} · {} · {}{from}",
        asset.name,
        size(bytes),
        asset.content_type
    ))
}

pub fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / (1 << 10) as f64),
        b => format!("{b} B"),
    }
}

pub fn sha256(file: &Path) -> Result<String, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

// The {assets} of the release notes: every asset as a download link with
// its size and checksum.
pub fn card(assets: &[Asset], download_url: &str) -> Result<String, String> {
    if assets.is_empty() {
        return Ok(String::new());
    }
    let mut rows = Vec::new();
    for asset in assets {
        let bytes = std::fs::metadata(&asset.file)
            .map_err(|e| format!("{}: {e}", asset.file.display()))?
            .len();
        let title = asset.label.as_deref().unwrap_or(&asset.name);
        rows.push(format!(
            "| [{title}]({download_url}/{}) | {} | `{}` |",
            asset.name,
            size(bytes),
            sha256(&asset.file)?
        ));
    }
    Ok(format!(
        "### 📦\u{a0} Downloads\n\n| File | Size | SHA-256 |\n| --- | ---: | --- |\n{}",
        rows.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_json_with_metadata() {
        let assets = parse(&[
            "dist/a.tgz, dist/b c.zip\ndist/relcut".into(),
            r#"[{"file": "dist/x", "name": "x-linux", "label": "Linux x86_64"}, {"file": "notes.md", "content_type": "text/markdown"}]"#.into(),
        ])
        .unwrap();
        let names: Vec<_> = assets
            .iter()
            .map(|a| (a.name.as_str(), a.content_type.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("a.tgz", "application/gzip"),
                ("b c.zip", "application/zip"),
                ("relcut", "application/octet-stream"),
                ("x-linux", "application/octet-stream"),
                ("notes.md", "text/markdown"),
            ]
        );
        assert_eq!(assets[3].label.as_deref(), Some("Linux x86_64"));
        assert_eq!(
            content_type("relcut.cdx.json"),
            "application/vnd.cyclonedx+json"
        );
        assert_eq!(content_type("relcut.spdx.json"), "application/spdx+json");
        assert!(
            parse(&[r#"{"path": "x"}"#.into()])
                .unwrap_err()
                .contains("has no \"file\"")
        );
        assert!(
            parse(&[r#"{"file": "x", "mime": "y"}"#.into()])
                .unwrap_err()
                .contains("unknown key \"mime\"")
        );
    }

    #[test]
    fn card_links_sizes_and_checksums() {
        let dir = std::env::temp_dir().join(format!("relcut-card-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "abc").unwrap();
        let mut asset = Asset::new(dir.join("a.txt"));
        asset.label = Some("The A".into());
        let card = card(&[asset], "https://github.com/o/r/releases/download/v1.0.0").unwrap();
        assert!(card.contains("| [The A](https://github.com/o/r/releases/download/v1.0.0/a.txt) | 3 B | `ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad` |"), "{card}");
        assert_eq!(super::card(&[], "x").unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
