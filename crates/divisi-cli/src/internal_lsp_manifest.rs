//! Generates the `.claude-plugin/marketplace.json` entry for `divisi-lsp`
//! from SingleCLI's own LSP registry — the same registry `divisi-lsp`'s
//! `proxy::Router` reads at runtime, so the manifest's `extensionToLanguage`
//! map and the proxy's actual routing table can never drift apart as long
//! as this generator is re-run after registry changes (`single lsp add`,
//! `single lsp enable`, ...) — see `single lsp --help`'s note on
//! `install-integrations` needing a re-run after registry edits, same idea.
//!
//! Only dot-extension entries make it into the generated map — Claude
//! Code's real marketplace schema rejects a bare filename key (confirmed
//! live via `claude plugin validate`), so the registry's filename-keyed
//! presets (`dockerfile`, `meson`, `nginx`, `dockercompose`) are routable
//! by `divisi-lsp`'s own `Router` but not reachable through this plugin
//! mechanism — a real, confirmed platform limitation, not a bug in the
//! proxy's routing logic.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use divisi_protocol::LspServerSpec;
use std::path::Path;

pub fn generate(specs: &[LspServerSpec]) -> Value {
    let mut extension_to_language: Map<String, Value> = Map::new();
    for spec in specs.iter().filter(|s| s.enabled) {
        for ext in &spec.extensions {
            // Claude Code's real marketplace schema rejects `extensionToLanguage`
            // keys that aren't a dot-prefixed extension (confirmed via
            // `claude plugin validate`: a bare filename key like "Dockerfile"
            // fails with "lspServers: Invalid input"). divisi-lsp's own
            // `Router` supports exact-filename routing internally (for presets
            // like `dockerfile`/`meson`/`nginx`/`dockercompose`), but that
            // routing can never be reached through this plugin manifest — skip
            // those entries here rather than generating a manifest Claude Code
            // refuses to install.
            if !ext.starts_with('.') {
                continue;
            }
            extension_to_language.entry(ext.clone()).or_insert_with(|| Value::String(spec.name.clone()));
        }
    }
    json!({
        "$schema": "https://json.schemastore.org/claude-code-marketplace.json",
        "name": "single-lsp-marketplace",
        "description": "Dynamic LSP proxy for SingleCLI's unified language server registry.",
        "owner": { "name": "Navin B. Ruas", "email": "founder@nbr.company" },
        "plugins": [
            {
                "name": "divisi-lsp",
                "description": "Dynamically proxies to whichever real language server SingleCLI's registry maps your open file's extension to.",
                "version": "0.1.0",
                "author": { "name": "Navin B. Ruas", "email": "founder@nbr.company" },
                "source": "./plugins/divisi-lsp",
                "category": "development",
                "strict": false,
                "lspServers": {
                    "divisi-lsp": {
                        "command": "divisi-lsp",
                        "args": [],
                        "extensionToLanguage": extension_to_language
                    }
                }
            }
        ]
    })
}

pub fn write_to(output_dir: &Path, specs: &[LspServerSpec]) -> Result<()> {
    let manifest_dir = output_dir.join(".claude-plugin");
    std::fs::create_dir_all(&manifest_dir).with_context(|| format!("creating {}", manifest_dir.display()))?;
    let manifest_path = manifest_dir.join("marketplace.json");
    let manifest = generate(specs);
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?).with_context(|| format!("writing {}", manifest_path.display()))?;

    let plugin_dir = output_dir.join("plugins").join("divisi-lsp");
    std::fs::create_dir_all(&plugin_dir).with_context(|| format!("creating {}", plugin_dir.display()))?;
    std::fs::write(
        plugin_dir.join("README.md"),
        "# divisi-lsp\n\nDynamic LSP proxy — see SingleCLI's docs/superpowers/specs/2026-08-24-claude-code-singlecli-integration-design.md.\n",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, exts: &[&str]) -> LspServerSpec {
        LspServerSpec { name: name.to_string(), command: name.to_string(), args: Vec::new(), extensions: exts.iter().map(|e| e.to_string()).collect(), enabled: true }
    }

    #[test]
    fn generates_extension_to_language_from_enabled_presets_only() {
        let mut disabled = spec("elm", &[".elm"]);
        disabled.enabled = false;
        let manifest = generate(&[spec("rust-analyzer", &[".rs"]), disabled]);
        let ext_map = &manifest["plugins"][0]["lspServers"]["divisi-lsp"]["extensionToLanguage"];
        assert_eq!(ext_map[".rs"], "rust-analyzer");
        assert!(ext_map.get(".elm").is_none());
    }

    #[test]
    fn skips_filename_keyed_presets_claude_code_would_reject() {
        let manifest = generate(&[spec("dockerfile", &["Dockerfile"]), spec("rust-analyzer", &[".rs"])]);
        let ext_map = &manifest["plugins"][0]["lspServers"]["divisi-lsp"]["extensionToLanguage"];
        assert!(ext_map.get("Dockerfile").is_none(), "a bare filename key must never reach the generated manifest");
        assert_eq!(ext_map.as_object().unwrap().len(), 1, "only the dot-extension entry should survive");
        assert_eq!(ext_map[".rs"], "rust-analyzer");
    }

    #[test]
    fn write_to_creates_manifest_and_plugin_stub() {
        let dir = tempfile::tempdir().unwrap();
        write_to(dir.path(), &[spec("gopls", &[".go"])]).unwrap();
        assert!(dir.path().join(".claude-plugin/marketplace.json").exists());
        assert!(dir.path().join("plugins/divisi-lsp/README.md").exists());
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(".claude-plugin/marketplace.json")).unwrap()).unwrap();
        assert_eq!(manifest["plugins"][0]["lspServers"]["divisi-lsp"]["extensionToLanguage"][".go"], "gopls");
    }
}
