//! Reads/writes `~/.config/opencode/opencode.jsonc`'s `mcp` key. Format
//! confirmed by direct inspection of a real file: `"type": "local"`,
//! `"command": [...]` (array, not a single string), `"environment": {...}`,
//! `"enabled": bool`.
//!
//! JSONC allows `//` and `/* */` comments that plain `serde_json` can't
//! parse. Phase 1 handles this with a minimal comment stripper below rather
//! than pulling in a JSONC crate — it is deliberately conservative (only
//! strips comments outside string literals) but, like the TOML writer,
//! re-serializing loses comments and reformats the file. This is an adapter
//! *emulation*, not a native capability — documented here and in
//! docs/architecture.md rather than silently discarding user comments.

use divisi_protocol::{LspServerSpec, McpServerSpec, ProviderSpec};
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::Path;

/// Strips `//line` and `/* block */` comments that are not inside a JSON
/// string literal, so the rest can be parsed with `serde_json`.
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let stripped = strip_jsonc_comments(&text);
        serde_json::from_str(&stripped)
            .with_context(|| format!("parsing {} as JSONC", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("opencode.jsonc root is not a JSON object")?;
    let mcp_obj = root_obj
        .entry("mcp")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("mcp is not a JSON object")?;

    for server in servers {
        let mut command_parts = vec![server.command.clone()];
        command_parts.extend(server.args.iter().cloned());

        let mut entry = Map::new();
        entry.insert("type".into(), Value::String("local".into()));
        entry.insert("command".into(), Value::Array(command_parts.into_iter().map(Value::String).collect()));
        if !server.env.is_empty() {
            let env: Map<String, Value> =
                server.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
            entry.insert("environment".into(), Value::Object(env));
        }
        entry.insert("enabled".into(), Value::Bool(server.enabled));
        mcp_obj.insert(server.name.clone(), Value::Object(entry));
    }

    Ok(root)
}

/// Removes only the named servers from `mcp`, leaving anything else
/// (including `lsp`) untouched.
pub fn remove(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let stripped = strip_jsonc_comments(&text);
    let mut root: Value =
        serde_json::from_str(&stripped).with_context(|| format!("parsing {} as JSONC", path.display()))?;
    let root_obj = root.as_object_mut().context("opencode.jsonc root is not a JSON object")?;
    if let Some(mcp_obj) = root_obj.get_mut("mcp").and_then(|v| v.as_object_mut()) {
        for name in names {
            mcp_obj.remove(name);
        }
    }
    Ok(Some(root))
}

/// Writes SingleCLI's LSP registry into `opencode.jsonc`'s `lsp` key,
/// keyed by name with a `"command"` array — the one shape this project has
/// directly observed in a real `opencode.jsonc` (`{"dockerfile":
/// {"command": ["docker-langserver","--stdio"]}}`). Only `enabled`
/// servers are written; `opencode.jsonc`'s `lsp` entries have no confirmed
/// per-entry enable/disable field, so a disabled registry entry is simply
/// omitted rather than guessing one.
///
/// `extensions` **is** written (as of opencode 1.18.18, confirmed live):
/// omitting it used to be the documented, deliberate choice here — this
/// project hadn't confirmed the field was read — but current opencode
/// now refuses to start at all without it ("For custom LSP servers,
/// 'extensions' array is required"), so an entry with no extensions
/// isn't just incomplete, it's a hard startup failure for every command,
/// not only LSP ones. Real values already exist on `LspServerSpec` for
/// this — `divisi_core::lsp`'s presets — so this is filling in a field
/// that's genuinely there, not guessing one.
pub fn apply_lsp(path: &Path, servers: &[LspServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let stripped = strip_jsonc_comments(&text);
        serde_json::from_str(&stripped)
            .with_context(|| format!("parsing {} as JSONC", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("opencode.jsonc root is not a JSON object")?;
    let lsp_obj = root_obj
        .entry("lsp")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("lsp is not a JSON object")?;

    for server in servers {
        if !server.enabled {
            lsp_obj.remove(&server.name);
            continue;
        }
        let mut command_parts = vec![server.command.clone()];
        command_parts.extend(server.args.iter().cloned());

        let mut entry = Map::new();
        entry.insert("command".into(), Value::Array(command_parts.into_iter().map(Value::String).collect()));
        entry.insert("extensions".into(), Value::Array(server.extensions.iter().cloned().map(Value::String).collect()));
        lsp_obj.insert(server.name.clone(), Value::Object(entry));
    }

    Ok(root)
}

/// Writes a custom/local provider into `opencode.jsonc`'s `provider.<name>`
/// key, in opencode's own documented custom-OpenAI-compatible-provider
/// format (confirmed via opencode's docs, not guessed):
/// `{"npm": "@ai-sdk/openai-compatible", "name": ..., "options": {"baseURL":
/// ..., "apiKey": "{env:VAR}"}, "models": {...}}`. The `apiKey` value is
/// always the literal env-var-reference string `"{env:VAR}"`, never a raw
/// secret — opencode resolves that reference from its own process
/// environment at runtime, which SingleCLI populates separately via
/// `divisi_core::provider_keys::resolve_env_for_agent`. This function
/// never sees or needs the actual secret value.
/// Well-known models.dev registry providers that back an OpenAI-compatible
/// API — used only when opencode's own registry cache isn't on disk yet.
/// Not exhaustive (models.dev has 200+ entries); the cache read below is
/// the real check.
const KNOWN_REGISTRY_PROVIDERS: &[&str] = &[
    "nvidia", "openrouter", "mistral", "cerebras", "groq", "deepinfra", "togetherai",
    "fireworks-ai", "nebius", "chutes", "perplexity", "xai", "openai", "anthropic",
    "google", "cohere", "baseten", "novita-ai", "hyperbolic", "siliconflow", "venice",
    "requesty", "nano-gpt", "huggingface", "azure",
];

/// The key to write a custom provider under in `opencode.jsonc`.
///
/// opencode merges every configured provider's models with **models.dev's
/// registry catalog** (cached at `~/.cache/opencode/models.json`). So a
/// provider written under a key that *is* a models.dev provider — `nvidia`,
/// `openrouter`, … — gets that provider's entire registry catalog (100+
/// entries, including EOL'd models) merged on top of the one or two models
/// SingleCLI actually curated, and opencode's "auto" selection can then
/// pick a dead model. Writing it under `single-<name>` instead (SingleCLI's
/// existing pool namespace, which isn't in models.dev) leaves only the
/// declared models. `--output-format`/`autoload` do not prevent the merge —
/// verified against opencode 1.18.29.
pub fn opencode_provider_key(config_path: &Path, provider_name: &str) -> String {
    if provider_name.starts_with("single-") {
        return provider_name.to_string(); // already namespaced
    }
    let collides = registry_provider_names(config_path)
        .map(|names| names.iter().any(|n| n == provider_name))
        .unwrap_or_else(|| KNOWN_REGISTRY_PROVIDERS.contains(&provider_name));
    if collides {
        format!("single-{provider_name}")
    } else {
        provider_name.to_string()
    }
}

/// Top-level keys of opencode's models.dev registry cache
/// (`<home>/.cache/opencode/models.json`), derived from the
/// `.../.config/opencode/opencode.jsonc` path. `None` if the cache isn't
/// present or can't be parsed.
fn registry_provider_names(config_path: &Path) -> Option<Vec<String>> {
    // opencode.jsonc -> opencode -> .config -> <home>
    let home = config_path.parent()?.parent()?.parent()?;
    let cache = home.join(".cache").join("opencode").join("models.json");
    let text = std::fs::read_to_string(cache).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(value.as_object()?.keys().cloned().collect())
}

pub fn apply_provider(path: &Path, provider: &ProviderSpec) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let stripped = strip_jsonc_comments(&text);
        serde_json::from_str(&stripped)
            .with_context(|| format!("parsing {} as JSONC", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("opencode.jsonc root is not a JSON object")?;
    let provider_obj = root_obj
        .entry("provider")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("provider is not a JSON object")?;

    let mut models = Map::new();
    for model in &provider.models {
        let mut model_entry = Map::new();
        model_entry.insert("name".into(), Value::String(model.name.clone()));
        models.insert(model.id.clone(), Value::Object(model_entry));
    }

    let mut options = Map::new();
    if let Some(base_url) = &provider.base_url {
        options.insert("baseURL".into(), Value::String(base_url.clone()));
    }
    options.insert("apiKey".into(), Value::String(format!("{{env:{}}}", provider.env_var_name)));

    let mut entry = Map::new();
    entry.insert("npm".into(), Value::String("@ai-sdk/openai-compatible".into()));
    entry.insert("name".into(), Value::String(provider.name.clone()));
    entry.insert("options".into(), Value::Object(options));
    entry.insert("models".into(), Value::Object(models));

    let key = opencode_provider_key(path, &provider.name);
    if key != provider.name {
        // clear a stale bare-name block a previous sync wrote — it was
        // pulling the whole models.dev catalog for this provider.
        provider_obj.remove(&provider.name);
    }
    provider_obj.insert(key, Value::Object(entry));

    Ok(root)
}

/// Removes only the named servers from `lsp`, leaving `mcp` and anything
/// else untouched.
pub fn remove_lsp(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let stripped = strip_jsonc_comments(&text);
    let mut root: Value =
        serde_json::from_str(&stripped).with_context(|| format!("parsing {} as JSONC", path.display()))?;
    let root_obj = root.as_object_mut().context("opencode.jsonc root is not a JSON object")?;
    if let Some(lsp_obj) = root_obj.get_mut("lsp").and_then(|v| v.as_object_mut()) {
        for name in names {
            lsp_obj.remove(name);
        }
    }
    Ok(Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn strips_line_and_block_comments_outside_strings() {
        let input = "{\n  // a comment\n  \"a\": \"http://not-a-comment\", /* block */ \"b\": 1\n}";
        let stripped = strip_jsonc_comments(input);
        let value: Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["a"], "http://not-a-comment");
        assert_eq!(value["b"], 1);
    }

    #[test]
    fn adds_server_to_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let servers = vec![McpServerSpec {
            name: "git".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-git".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert_eq!(result["mcp"]["git"]["command"][0], "uvx");
        assert_eq!(result["mcp"]["git"]["command"][1], "mcp-server-git");
        assert_eq!(result["mcp"]["git"]["type"], "local");
    }

    #[test]
    fn parses_real_jsonc_with_comments_and_preserves_lsp_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(
            &path,
            "{\n  // lsp servers\n  \"lsp\": { \"rust\": {} },\n  \"mcp\": {}\n}",
        )
        .unwrap();
        let servers = vec![McpServerSpec {
            name: "memory".into(),
            command: "npx".into(),
            args: vec![],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert!(result["lsp"]["rust"].is_object());
        assert_eq!(result["mcp"]["memory"]["type"], "local");
    }

    #[test]
    fn apply_lsp_writes_command_array_and_skips_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let servers = vec![
            LspServerSpec { name: "rust-analyzer".into(), command: "rust-analyzer".into(), args: vec![], extensions: vec![".rs".into()], enabled: true },
            LspServerSpec { name: "pyright".into(), command: "pyright-langserver".into(), args: vec!["--stdio".into()], extensions: vec![".py".into()], enabled: false },
        ];
        let result = apply_lsp(&path, &servers).unwrap();
        assert_eq!(result["lsp"]["rust-analyzer"]["command"][0], "rust-analyzer");
        // Regression test: opencode 1.18.18 refuses to start at all
        // without this ("For custom LSP servers, 'extensions' array is
        // required") — confirmed live against a real install.
        assert_eq!(result["lsp"]["rust-analyzer"]["extensions"][0], ".rs");
        assert!(result["lsp"].get("pyright").is_none());
    }

    #[test]
    fn apply_lsp_preserves_mcp_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, "{\n  \"mcp\": { \"git\": { \"type\": \"local\" } }\n}").unwrap();
        let servers = vec![LspServerSpec { name: "gopls".into(), command: "gopls".into(), args: vec![], extensions: vec![".go".into()], enabled: true }];
        let result = apply_lsp(&path, &servers).unwrap();
        assert_eq!(result["mcp"]["git"]["type"], "local");
        assert_eq!(result["lsp"]["gopls"]["command"][0], "gopls");
    }

    #[test]
    fn remove_lsp_removes_only_named_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, "{\n  \"lsp\": { \"gopls\": {}, \"rust-analyzer\": {} }\n}").unwrap();
        let result = remove_lsp(&path, &["gopls".to_string()]).unwrap().unwrap();
        assert!(result["lsp"].get("gopls").is_none());
        assert!(result["lsp"]["rust-analyzer"].is_object());
    }

    #[test]
    fn apply_provider_writes_the_confirmed_opencode_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let provider = ProviderSpec {
            name: "omniroute".into(),
            env_var_name: "OMNIROUTE_API_KEY".into(),
            secret_name: "provider:omniroute".into(),
            base_url: Some("http://localhost:20128/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "auto".into(), name: "Auto (best available)".into() }],
        };
        let result = apply_provider(&path, &provider).unwrap();
        assert_eq!(result["provider"]["omniroute"]["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(result["provider"]["omniroute"]["name"], "omniroute");
        assert_eq!(result["provider"]["omniroute"]["options"]["baseURL"], "http://localhost:20128/v1");
        assert_eq!(result["provider"]["omniroute"]["options"]["apiKey"], "{env:OMNIROUTE_API_KEY}");
        assert_eq!(result["provider"]["omniroute"]["models"]["auto"]["name"], "Auto (best available)");
    }

    #[test]
    fn apply_provider_never_writes_a_raw_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let provider = ProviderSpec {
            name: "testprov".into(),
            env_var_name: "TESTPROV_API_KEY".into(),
            secret_name: "provider:testprov".into(),
            base_url: None,
            models: vec![divisi_protocol::ModelSpec { id: "m1".into(), name: "Model One".into() }],
        };
        let result = apply_provider(&path, &provider).unwrap();
        let rendered = serde_json::to_string(&result).unwrap();
        assert!(rendered.contains("{env:TESTPROV_API_KEY}"));
        assert!(!rendered.contains("sk-"), "no raw secret-shaped string should ever appear");
    }

    #[test]
    fn apply_provider_preserves_existing_mcp_and_lsp_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, "{\n  \"mcp\": { \"git\": { \"type\": \"local\" } },\n  \"lsp\": { \"rust\": {} }\n}").unwrap();
        let provider = ProviderSpec {
            name: "omniroute".into(),
            env_var_name: "OMNIROUTE_API_KEY".into(),
            secret_name: "provider:omniroute".into(),
            base_url: Some("http://localhost:20128/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "auto".into(), name: "Auto".into() }],
        };
        let result = apply_provider(&path, &provider).unwrap();
        assert_eq!(result["mcp"]["git"]["type"], "local");
        assert!(result["lsp"]["rust"].is_object());
        assert!(result["provider"]["omniroute"].is_object());
    }

    #[test]
    fn apply_provider_omits_base_url_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let provider = ProviderSpec {
            name: "testprov".into(),
            env_var_name: "TESTPROV_API_KEY".into(),
            secret_name: "provider:testprov".into(),
            base_url: None,
            models: vec![divisi_protocol::ModelSpec { id: "m1".into(), name: "Model One".into() }],
        };
        let result = apply_provider(&path, &provider).unwrap();
        assert!(result["provider"]["testprov"]["options"].get("baseURL").is_none());
    }

    #[test]
    fn opencode_provider_key_aliases_registry_collisions_via_the_denylist() {
        let p = Path::new("/nonexistent/.config/opencode/opencode.jsonc");
        assert_eq!(opencode_provider_key(p, "nvidia"), "single-nvidia");
        assert_eq!(opencode_provider_key(p, "openrouter"), "single-openrouter");
        assert_eq!(opencode_provider_key(p, "my-private-llm"), "my-private-llm");
        assert_eq!(opencode_provider_key(p, "single-nvidia"), "single-nvidia"); // already namespaced
    }

    #[test]
    fn opencode_provider_key_reads_opencodes_registry_cache_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".config").join("opencode").join("opencode.jsonc");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        let cache = dir.path().join(".cache").join("opencode").join("models.json");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, r#"{ "acmecloud": { "models": {} }, "widgetsai": {} }"#).unwrap();

        // in the cache -> aliased; not in the cache and not in the
        // denylist -> left alone (even though the denylist would catch it).
        assert_eq!(opencode_provider_key(&cfg, "acmecloud"), "single-acmecloud");
        assert_eq!(opencode_provider_key(&cfg, "widgetsai"), "single-widgetsai");
        assert_eq!(opencode_provider_key(&cfg, "nvidia"), "nvidia"); // cache is authoritative here
    }

    #[test]
    fn apply_provider_writes_under_the_alias_and_removes_a_stale_bare_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        // a prior sync left a polluting bare `nvidia` block.
        std::fs::write(&path, r#"{ "provider": { "nvidia": { "npm": "@ai-sdk/openai-compatible", "models": { "stale/model": {} } } } }"#).unwrap();
        let provider = ProviderSpec {
            name: "nvidia".into(),
            env_var_name: "NVIDIA_API_KEY".into(),
            secret_name: "provider:nvidia".into(),
            base_url: Some("https://integrate.api.nvidia.com/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "deepseek-ai/deepseek-v4-flash-0731".into(), name: "DeepSeek V4 Flash".into() }],
        };
        let result = apply_provider(&path, &provider).unwrap();
        assert!(result["provider"].get("nvidia").is_none(), "stale bare key must be removed");
        let block = &result["provider"]["single-nvidia"];
        assert_eq!(block["name"], "nvidia");
        assert_eq!(block["options"]["baseURL"], "https://integrate.api.nvidia.com/v1");
        assert!(block["models"].get("deepseek-ai/deepseek-v4-flash-0731").is_some());
        assert!(block["models"].get("stale/model").is_none());
    }
}
