use std::collections::BTreeMap;

use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct TreeNode {
    name: String,
    path: String,
    folders: BTreeMap<String, TreeNode>,
    files: Vec<Value>,
}

impl TreeNode {
    fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            folders: BTreeMap::new(),
            files: Vec::new(),
        }
    }

    fn into_json(mut self) -> Value {
        let mut folders: Vec<(String, TreeNode)> = self.folders.into_iter().collect();
        folders.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        self.files.sort_by(|a, b| {
            let ap = a.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let bp = b.get("path").and_then(|v| v.as_str()).unwrap_or("");
            ap.cmp(bp)
        });
        json!({
            "name": self.name,
            "path": self.path,
            "folders": folders.into_iter().map(|(_, child)| child.into_json()).collect::<Vec<_>>(),
            "files": self.files,
        })
    }
}

/// Build nested folder tree structure identical to `macro_io/tree.py:build_macro_tree`.
pub fn build_macro_tree(items: &[Value]) -> Value {
    let mut root = TreeNode::new("宏", "宏");

    for item in items {
        let rel = item
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }

        let mut node = &mut root;
        let mut current_parts = Vec::new();
        for &part in &parts[..parts.len() - 1] {
            current_parts.push(part);
            let folder_path = current_parts.join("/");
            node = node
                .folders
                .entry(part.to_string())
                .or_insert_with(|| TreeNode::new(part, folder_path));
        }

        node.files.push(json!({
            "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(parts[parts.len() - 1]),
            "path": rel,
            "size": item.get("size").cloned().unwrap_or(Value::Null),
            "mtime": item.get("mtime").cloned().unwrap_or(Value::Null),
        }));
    }

    root.into_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_groups_subfolders_and_sorts() {
        let items = vec![
            json!({"path": "宏/b.json", "name": "b.json", "size": 10, "mtime": 1}),
            json!({"path": "宏/录制宏/a.json", "name": "a.json", "size": 20, "mtime": 2}),
        ];
        let tree = build_macro_tree(&items);
        let root_folders = tree["folders"].as_array().unwrap();
        assert_eq!(root_folders[0]["name"], "宏");
        assert_eq!(root_folders[0]["files"][0]["path"], "宏/b.json");
        assert_eq!(
            root_folders[0]["folders"][0]["files"][0]["path"],
            "宏/录制宏/a.json"
        );
    }
}
