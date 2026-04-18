use std::path::{Path, PathBuf};

pub fn module_path_for_file(path: &str) -> String {
    module_scope_for_file(path).join("::")
}

pub fn module_scope_for_file(path: &str) -> Vec<String> {
    let normalized = path.replace('\\', "/");
    let path = Path::new(&normalized);
    let mut parts = Vec::new();
    for component in path.components() {
        let value = component.as_os_str().to_string_lossy();
        if value == "src"
            || value == "tests"
            || value == "benches"
            || value == "examples"
        {
            continue;
        }
        if value.ends_with(".rs") {
            let stem = value.trim_end_matches(".rs");
            if stem != "mod" && stem != "lib" && stem != "main" {
                parts.push(stem.to_string());
            }
            break;
        }
        parts.push(value.to_string());
    }
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

pub fn resolve_mod_declaration(file: &str, module_name: &str) -> Vec<String> {
    let normalized = file.replace('\\', "/");
    let parent = Path::new(&normalized).parent().unwrap_or_else(|| Path::new(""));
    let base = PathBuf::from(parent);
    [
        base.join(format!("{module_name}.rs")),
        base.join(module_name).join("mod.rs"),
    ]
    .into_iter()
    .map(|path| path.to_string_lossy().replace('\\', "/"))
    .collect()
}
