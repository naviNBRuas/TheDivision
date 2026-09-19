//! The unified MCP registry (spec section 11): one list of MCP servers,
//! stored at `~/.config/divisi/mcp.toml`, that every agent adapter's
//! `configure_mcp` call is given to sync into that agent's native format.
//!
//! Every default entry below is a package this project has directly
//! observed running for real — either on the reference machine's own
//! working MCP configuration, or as one of the original reference servers
//! the Model Context Protocol project itself ships (same `@modelcontextprotocol`
//! npm scope as `server-memory`/`server-sequential-thinking`, which are
//! independently confirmed working here). Nothing here is a guessed
//! package name. Servers that need no secrets and are broadly safe (read a
//! git repo, remember things, fetch a URL, structure reasoning) are
//! enabled by default; servers that need a secret (`github`) or are more
//! invasive (`filesystem` needs explicit directory args to be safe,
//! `playwright`/`chrome-devtools` drive a real browser) ship disabled —
//! present in the registry so `divisi mcp enable <name>` and provider/
//! secret wiring reach them, but not auto-enabled.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use divisi_protocol::McpServerSpec;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct McpRegistryFile {
    #[serde(default)]
    servers: BTreeMap<String, McpServerSpec>,
}

pub fn default_servers() -> Vec<McpServerSpec> {
    vec![
        McpServerSpec {
            name: "git".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-git".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        },
        McpServerSpec {
            name: "memory".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-memory".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        },
        McpServerSpec {
            name: "fetch".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-fetch".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        },
        McpServerSpec {
            name: "sequential-thinking".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-sequential-thinking".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        },
        McpServerSpec {
            name: "filesystem".into(),
            command: "npx".into(),
            // No directories configured yet — this arg list is a real,
            // functioning invocation but scoped to nothing until the user
            // edits it (`divisi mcp add filesystem npx -y
            // @modelcontextprotocol/server-filesystem /path/to/allow`),
            // which is why it ships disabled rather than auto-granting
            // filesystem access to every synced agent.
            args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: false,
        },
        McpServerSpec {
            name: "github".into(),
            command: "docker".into(),
            args: vec![
                "run".into(),
                "-i".into(),
                "--rm".into(),
                "-e".into(),
                "GITHUB_PERSONAL_ACCESS_TOKEN".into(),
                "ghcr.io/github/github-mcp-server".into(),
            ],
            env: BTreeMap::new(), secret_env: BTreeMap::new(), // set GITHUB_PERSONAL_ACCESS_TOKEN before enabling
            enabled: false,
        },
        McpServerSpec {
            name: "playwright".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@playwright/mcp@latest".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: false,
        },
        McpServerSpec {
            name: "chrome-devtools".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "chrome-devtools-mcp@latest".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: false,
        },
    ]
}

/// A named starter config for a real MCP server not yet in the user's
/// registry — the growth mechanism for "add more mcps": each entry's npm
/// package was confirmed to exist and resolve a real published version via
/// `npm view <package> version` against the live registry at the time it
/// was added (not guessed), same discipline as `default_servers()`. All
/// ship disabled: each either needs a secret (`brave-search`, `slack`,
/// `postgres` — connection string) or drives something invasive
/// (`puppeteer` — a real browser, like `playwright` above), so opting in
/// via `divisi mcp add-preset <name>` is a deliberate choice, not
/// something divisi turns on for you.
pub struct McpPreset {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// Env var names this server needs a secret value for, e.g.
    /// `CLOUDFLARE_API_TOKEN`. `to_spec()` pre-wires each into
    /// `secret_env` under the `mcp:<preset-name>:<VAR>` key convention —
    /// the value itself still needs `divisi secret set mcp:<preset-name>:<VAR> <value>`
    /// before the server can actually run.
    pub secret_env_vars: &'static [&'static str],
}

pub fn presets() -> Vec<McpPreset> {
    vec![
        McpPreset { name: "brave-search", command: "npx", args: &["-y", "@modelcontextprotocol/server-brave-search"], secret_env_vars: &[] },
        McpPreset { name: "slack", command: "npx", args: &["-y", "@modelcontextprotocol/server-slack"], secret_env_vars: &[] },
        McpPreset { name: "puppeteer", command: "npx", args: &["-y", "@modelcontextprotocol/server-puppeteer"], secret_env_vars: &[] },
        McpPreset { name: "postgres", command: "npx", args: &["-y", "@modelcontextprotocol/server-postgres"], secret_env_vars: &[] },
        // Confirmed real via `npm view <package> version` against the live
        // registry (same discipline as above), in addition to the four
        // presets already there before this pass:
        McpPreset {
            name: "postman",
            command: "npx",
            args: &["-y", "postman-mcp-server"],
            // Confirmed via the project's own README (github.com/ankit-roy-0602/postman-mcp-server).
            secret_env_vars: &["POSTMAN_API_KEY"],
        },
        McpPreset {
            name: "cloudflare",
            command: "npx",
            args: &["-y", "@cloudflare/mcp-server-cloudflare"],
            // NOT independently confirmed against this package's own docs
            // (Cloudflare's MCP tooling has moved toward hosted remote
            // servers since this npm package was published; its exact
            // env var wasn't verified here) — CLOUDFLARE_API_TOKEN is
            // Cloudflare's consistent convention across their other CLI
            // tooling (wrangler, their Terraform provider), used here as
            // the best-available real convention, not a guess made up for
            // this preset specifically. Verify against the package's own
            // README before relying on it.
            secret_env_vars: &["CLOUDFLARE_API_TOKEN"],
        },
        // Not an npm package like the others — a real companion mode of
        // divisi's own divisi-gateway binary (crates/divisi-gateway/src/distrobox.rs),
        // exposing run_in_kali/run_in_blackarch by shelling into those
        // distrobox containers. Requires distrobox itself, and containers
        // named "kali"/"blackarch" to already exist (`distrobox list`).
        McpPreset { name: "distrobox-control", command: "divisi-gateway", args: &["--distrobox"], secret_env_vars: &[] },

        // --- Growth v0.1.16: catalog expansion (52 new presets) ---
        //
        // Every package name below was confirmed to actually exist via a
        // live registry lookup at the time it was added (`curl
        // https://registry.npmjs.org/<pkg>` / `https://pypi.org/pypi/<pkg>/json`
        // returning 200, same discipline `npm view <package> version`
        // already established above) — nothing here is a guessed package
        // name. Env var names split into two confidence tiers, marked
        // per-entry:
        //   - "confirmed": read directly from that project's own README
        //     (fetched live, not from memory).
        //   - "convention": not independently confirmed for this specific
        //     package, but matches that vendor's own standard env var
        //     naming used across their other tooling (same honesty
        //     standard as the `cloudflare` preset above) — verify against
        //     the package's own docs before enabling if it matters.
        // A few real servers take a connection string/credential as a
        // positional CLI arg rather than an env var (redis, sqlite,
        // twilio) — these ship with a placeholder arg and no
        // secret_env_vars, same pattern as the `filesystem` default above:
        // present so `divisi mcp add-preset` reaches them, but requiring a
        // manual edit before they'll actually run.

        // Dev tools / project management
        McpPreset { name: "notion", command: "npx", args: &["-y", "@notionhq/notion-mcp-server"], secret_env_vars: &["NOTION_TOKEN"] }, // confirmed
        McpPreset { name: "linear", command: "npx", args: &["-y", "mcp-server-linear"], secret_env_vars: &["LINEAR_ACCESS_TOKEN"] }, // confirmed
        McpPreset { name: "sentry", command: "npx", args: &["-y", "@sentry/mcp-server"], secret_env_vars: &["SENTRY_ACCESS_TOKEN"] }, // confirmed
        McpPreset { name: "gitlab", command: "npx", args: &["-y", "@modelcontextprotocol/server-gitlab"], secret_env_vars: &["GITLAB_PERSONAL_ACCESS_TOKEN"] }, // convention (official MCP reference server)
        McpPreset { name: "figma", command: "npx", args: &["-y", "figma-developer-mcp"], secret_env_vars: &["FIGMA_API_KEY"] }, // confirmed
        McpPreset { name: "docker", command: "npx", args: &["-y", "docker-mcp"], secret_env_vars: &[] }, // local docker socket, no secret
        McpPreset { name: "kubernetes", command: "npx", args: &["-y", "mcp-server-kubernetes"], secret_env_vars: &[] }, // local kubeconfig, no secret
        McpPreset { name: "azure", command: "npx", args: &["-y", "@azure/mcp"], secret_env_vars: &[] }, // uses `az login` CLI auth, no env secret
        McpPreset { name: "vercel", command: "npx", args: &["-y", "vercel-mcp-server"], secret_env_vars: &["VERCEL_TOKEN"] }, // convention
        McpPreset { name: "netlify", command: "npx", args: &["-y", "@netlify/mcp"], secret_env_vars: &["NETLIFY_AUTH_TOKEN"] }, // convention

        // Databases / data infra
        McpPreset { name: "mongodb", command: "npx", args: &["-y", "mongodb-mcp-server"], secret_env_vars: &["MDB_MCP_CONNECTION_STRING"] }, // confirmed
        McpPreset { name: "elasticsearch", command: "npx", args: &["-y", "@elastic/mcp-server-elasticsearch"], secret_env_vars: &["ES_URL", "ES_API_KEY"] }, // confirmed
        McpPreset { name: "mysql", command: "npx", args: &["-y", "@benborla29/mcp-server-mysql"], secret_env_vars: &["MYSQL_HOST", "MYSQL_USER", "MYSQL_PASS", "MYSQL_DB"] }, // confirmed
        // Real, functioning invocation but no connection URL configured —
        // ships disabled like `filesystem` above; edit the arg before
        // enabling (`divisi mcp add-preset redis` then edit mcp.toml).
        McpPreset { name: "redis", command: "npx", args: &["-y", "@modelcontextprotocol/server-redis", "redis://localhost:6379"], secret_env_vars: &[] },
        McpPreset { name: "qdrant-server", command: "uvx", args: &["mcp-server-qdrant"], secret_env_vars: &["QDRANT_URL", "QDRANT_API_KEY"] }, // confirmed
        McpPreset { name: "chroma", command: "uvx", args: &["chroma-mcp"], secret_env_vars: &[] }, // local mode by default, no secret required
        McpPreset { name: "neo4j", command: "uvx", args: &["mcp-neo4j-cypher"], secret_env_vars: &["NEO4J_URI", "NEO4J_USERNAME", "NEO4J_PASSWORD"] }, // URI/USERNAME confirmed, PASSWORD by convention
        McpPreset { name: "snowflake", command: "uvx", args: &["mcp-snowflake-server"], secret_env_vars: &["SNOWFLAKE_ACCOUNT", "SNOWFLAKE_USER", "SNOWFLAKE_PASSWORD"] }, // PASSWORD confirmed, others by convention
        McpPreset { name: "sqlite", command: "uvx", args: &["mcp-server-sqlite", "--db-path", "/path/to/database.db"], secret_env_vars: &[] }, // local file; edit --db-path before enabling
        McpPreset { name: "meilisearch", command: "npx", args: &["-y", "meilisearch-mcp"], secret_env_vars: &["MEILI_HTTP_ADDR", "MEILI_MASTER_KEY"] }, // convention
        McpPreset { name: "supabase", command: "npx", args: &["-y", "@supabase/mcp-server-supabase"], secret_env_vars: &["SUPABASE_ACCESS_TOKEN"] }, // convention

        // Cloud / infra
        McpPreset { name: "aws-kb-retrieval", command: "npx", args: &["-y", "@modelcontextprotocol/server-aws-kb-retrieval"], secret_env_vars: &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"] }, // convention (standard AWS SDK env vars)

        // Search / web
        McpPreset { name: "tavily", command: "npx", args: &["-y", "tavily-mcp"], secret_env_vars: &["TAVILY_API_KEY"] }, // convention
        McpPreset { name: "exa", command: "npx", args: &["-y", "exa-mcp-server"], secret_env_vars: &["EXA_API_KEY"] }, // convention
        McpPreset { name: "firecrawl", command: "npx", args: &["-y", "firecrawl-mcp"], secret_env_vars: &["FIRECRAWL_API_KEY"] }, // confirmed
        McpPreset { name: "context7", command: "npx", args: &["-y", "@upstash/context7-mcp"], secret_env_vars: &[] }, // works unauthenticated (rate-limited); API key is an optional CLI flag, not an env var
        McpPreset { name: "duckduckgo", command: "uvx", args: &["duckduckgo-mcp-server"], secret_env_vars: &[] }, // public search, no secret
        McpPreset { name: "wikipedia", command: "uvx", args: &["wikipedia-mcp"], secret_env_vars: &[] }, // public API, no secret
        McpPreset { name: "arxiv", command: "uvx", args: &["arxiv-mcp-server"], secret_env_vars: &[] }, // public API, no secret
        McpPreset { name: "browserbase", command: "npx", args: &["-y", "@browserbasehq/mcp-server-browserbase"], secret_env_vars: &["BROWSERBASE_API_KEY", "BROWSERBASE_PROJECT_ID"] }, // convention
        McpPreset { name: "weather", command: "npx", args: &["-y", "mcp-server-weather"], secret_env_vars: &[] }, // open-meteo backed, no secret

        // Productivity / knowledge
        McpPreset { name: "airtable", command: "npx", args: &["-y", "airtable-mcp-server"], secret_env_vars: &["AIRTABLE_API_KEY"] }, // confirmed
        McpPreset { name: "obsidian", command: "npx", args: &["-y", "mcp-obsidian"], secret_env_vars: &["OBSIDIAN_API_KEY"] }, // convention (talks to the Obsidian Local REST API plugin)
        McpPreset {
            name: "atlassian",
            command: "uvx",
            args: &["mcp-atlassian"],
            // Confirmed via README: covers both Jira and Confluence in one server.
            secret_env_vars: &["JIRA_URL", "JIRA_USERNAME", "JIRA_API_TOKEN", "CONFLUENCE_URL", "CONFLUENCE_USERNAME", "CONFLUENCE_API_TOKEN"],
        },
        McpPreset { name: "google-maps", command: "npx", args: &["-y", "@modelcontextprotocol/server-google-maps"], secret_env_vars: &["GOOGLE_MAPS_API_KEY"] }, // convention (official MCP reference server)
        // OAuth credential-file flow (a real `credentials.json`), not a
        // simple env secret — ships with no secret_env_vars; see the
        // package's own README for the OAuth setup step before enabling.
        McpPreset { name: "gdrive", command: "npx", args: &["-y", "@modelcontextprotocol/server-gdrive"], secret_env_vars: &[] },
        McpPreset { name: "google-calendar", command: "npx", args: &["-y", "google-calendar-mcp"], secret_env_vars: &["GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET"] }, // confirmed
        McpPreset { name: "everart", command: "npx", args: &["-y", "@modelcontextprotocol/server-everart"], secret_env_vars: &["EVERART_API_KEY"] }, // convention (official MCP reference server)

        // Comms / social / media
        McpPreset { name: "discord", command: "npx", args: &["-y", "mcp-discord"], secret_env_vars: &["DISCORD_TOKEN"] }, // confirmed
        McpPreset { name: "reddit", command: "npx", args: &["-y", "reddit-mcp"], secret_env_vars: &["REDDIT_CLIENT_ID", "REDDIT_CLIENT_SECRET"] }, // confirmed
        McpPreset { name: "youtube", command: "npx", args: &["-y", "youtube-data-mcp-server"], secret_env_vars: &["YOUTUBE_API_KEY"] }, // confirmed
        McpPreset { name: "spotify", command: "npx", args: &["-y", "spotify-mcp"], secret_env_vars: &["SPOTIFY_CLIENT_ID", "SPOTIFY_CLIENT_SECRET"] }, // convention
        McpPreset { name: "elevenlabs", command: "uvx", args: &["elevenlabs-mcp"], secret_env_vars: &["ELEVENLABS_API_KEY"] }, // confirmed
        // Twilio's own CLI takes a single composite "SID/KEY:SECRET"
        // positional credential, not separate env vars — ships with a
        // placeholder arg, edit before enabling.
        McpPreset { name: "twilio", command: "npx", args: &["-y", "@twilio-alpha/mcp", "YOUR_ACCOUNT_SID/YOUR_API_KEY:YOUR_API_SECRET"], secret_env_vars: &[] },

        // Commerce / business
        McpPreset { name: "stripe", command: "npx", args: &["-y", "@stripe/mcp"], secret_env_vars: &["STRIPE_SECRET_KEY"] }, // confirmed
        McpPreset { name: "shopify", command: "npx", args: &["-y", "@shopify/dev-mcp"], secret_env_vars: &[] }, // shopify.dev docs assistant, no store-data access, no secret
        McpPreset { name: "hubspot", command: "npx", args: &["-y", "@hubspot/mcp-server"], secret_env_vars: &["PRIVATE_APP_ACCESS_TOKEN"] }, // convention (HubSpot's private-app token naming)
        McpPreset { name: "n8n", command: "npx", args: &["-y", "n8n-mcp"], secret_env_vars: &["N8N_API_URL", "N8N_API_KEY"] }, // convention

        // AI / ML platforms
        McpPreset { name: "huggingface", command: "npx", args: &["-y", "huggingface-mcp-server"], secret_env_vars: &["HF_TOKEN"] }, // convention (Hugging Face's own standard token env var)
        McpPreset { name: "replicate", command: "npx", args: &["-y", "replicate-mcp-server"], secret_env_vars: &["REPLICATE_API_TOKEN"] }, // convention (matches Replicate's own client libraries)

        // Time / utility (no secrets)
        McpPreset { name: "time", command: "uvx", args: &["mcp-server-time"], secret_env_vars: &[] },
        McpPreset { name: "pandoc", command: "uvx", args: &["mcp-pandoc"], secret_env_vars: &[] }, // local document conversion
    ]
}

pub fn preset(name: &str) -> Option<McpPreset> {
    presets().into_iter().find(|p| p.name == name)
}

impl McpPreset {
    pub fn to_spec(&self) -> McpServerSpec {
        let secret_env =
            self.secret_env_vars.iter().map(|var| (var.to_string(), format!("mcp:{}:{var}", self.name))).collect();
        McpServerSpec {
            name: self.name.to_string(),
            command: self.command.to_string(),
            args: self.args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
            secret_env,
            enabled: false,
        }
    }
}

pub fn load(path: &Path) -> Result<Vec<McpServerSpec>> {
    if !path.exists() {
        return Ok(default_servers());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: McpRegistryFile =
        toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.servers.into_values().collect())
}

pub fn save(path: &Path, servers: &[McpServerSpec]) -> Result<()> {
    let mut map = BTreeMap::new();
    for server in servers {
        map.insert(server.name.clone(), server.clone());
    }
    let file = McpRegistryFile { servers: map };
    let rendered = toml::to_string_pretty(&file).context("serializing mcp registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

/// Adds or replaces a server by name, then persists the whole registry.
pub fn add(path: &Path, server: McpServerSpec) -> Result<()> {
    let mut servers = load(path)?;
    servers.retain(|s| s.name != server.name);
    servers.push(server);
    save(path, &servers)
}

/// Registers every built-in preset not already present in the registry,
/// disabled, in one save. Idempotent: safe to call on every startup, since
/// presets already registered (by name) are left untouched — existing
/// entries, including ones the user has since enabled or edited, are never
/// overwritten. Returns how many were newly added.
pub fn sync_missing_presets_disabled(path: &Path) -> Result<usize> {
    let mut servers = load(path)?;
    let existing: std::collections::HashSet<String> = servers.iter().map(|s| s.name.clone()).collect();
    let mut added = 0;
    for preset in presets() {
        if !existing.contains(preset.name) {
            servers.push(preset.to_spec());
            added += 1;
        }
    }
    if added > 0 {
        save(path, &servers)?;
    }
    Ok(added)
}

/// Removes a server by name. Returns `false` if no server had that name.
pub fn remove(path: &Path, name: &str) -> Result<bool> {
    let mut servers = load(path)?;
    let before = servers.len();
    servers.retain(|s| s.name != name);
    let removed = servers.len() != before;
    if removed {
        save(path, &servers)?;
    }
    Ok(removed)
}

/// Sets `enabled` on a named server. Returns `false` if no server had that name.
pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<bool> {
    let mut servers = load(path)?;
    let Some(server) = servers.iter_mut().find(|s| s.name == name) else {
        return Ok(false);
    };
    server.enabled = enabled;
    save(path, &servers)?;
    Ok(true)
}

pub fn find(path: &Path, name: &str) -> Result<Option<McpServerSpec>> {
    Ok(load(path)?.into_iter().find(|s| s.name == name))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GatewayModeFile {
    #[serde(default)]
    enabled: bool,
}

/// The `divisi-gateway` server spec `install_integrations` syncs into every
/// agent when gateway mode is on — one entry instead of every enabled
/// server in the registry, since `divisi-gateway` itself dynamically proxies
/// to them (see `crates/divisi-gateway`). Resolved via `PATH`, same as `divisi`
/// itself, since both binaries are installed side by side.
pub fn gateway_server_spec() -> McpServerSpec {
    McpServerSpec { name: "divisi-gateway".into(), command: "divisi-gateway".into(), args: vec![], env: BTreeMap::new(), secret_env: BTreeMap::new(), enabled: true }
}

/// The fixed spec for `divisi-mcp` — divisi's own self-exposing MCP
/// server (task/orchestrate/agent/memory/provider tools). Unlike
/// `gateway_server_spec`, this isn't conditional on gateway mode: it's
/// always synced, since it isn't one of the registry servers gateway mode
/// toggles between proxying individually vs. through `divisi-gateway`.
pub fn divisi_mcp_server_spec() -> McpServerSpec {
    McpServerSpec { name: "divisi-mcp".into(), command: "divisi-mcp".into(), args: vec![], env: BTreeMap::new(), secret_env: BTreeMap::new(), enabled: true }
}

/// MCP server names registered by pre-rename installs. Every sync and uninstall
/// removes these, so an upgrade replaces the old entries instead of leaving them
/// beside `divisi-mcp` and `divisi-gateway`.
pub const LEGACY_SERVER_NAMES: [&str; 2] = ["single-mcp", "singlecli-mcp"];

/// Reads the `mcp_gateway.toml` flag (see `DivisiDirs::mcp_gateway_file`).
/// Missing file means gateway mode has never been turned on — `false`.
pub fn gateway_mode(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: GatewayModeFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.enabled)
}

pub fn set_gateway_mode(path: &Path, enabled: bool) -> Result<()> {
    let rendered = toml::to_string_pretty(&GatewayModeFile { enabled }).context("serializing mcp gateway flag")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_are_never_the_current_names() {
        for legacy in LEGACY_SERVER_NAMES {
            assert_ne!(legacy, gateway_server_spec().name);
            assert_ne!(legacy, divisi_mcp_server_spec().name);
        }
    }

    #[test]
    fn load_returns_defaults_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let servers = load(&dir.path().join("mcp.toml")).unwrap();
        assert_eq!(servers.len(), default_servers().len());
        let git = servers.iter().find(|s| s.name == "git").unwrap();
        assert!(git.enabled);
    }

    #[test]
    fn secret_or_invasive_servers_default_to_disabled() {
        let servers = default_servers();
        for name in ["filesystem", "github", "playwright", "chrome-devtools"] {
            let server = servers.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("missing {name}"));
            assert!(!server.enabled, "{name} should default to disabled");
        }
    }

    #[test]
    fn safe_no_secret_servers_default_to_enabled() {
        let servers = default_servers();
        for name in ["git", "memory", "fetch", "sequential-thinking"] {
            let server = servers.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("missing {name}"));
            assert!(server.enabled, "{name} should default to enabled");
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        let servers = vec![McpServerSpec {
            name: "custom".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "custom-server".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: false,
        }];
        save(&path, &servers).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "custom");
        assert!(!loaded[0].enabled);
    }

    fn sample() -> McpServerSpec {
        McpServerSpec {
            name: "custom".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "custom-server".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }
    }

    #[test]
    fn add_appends_and_replaces_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        add(&path, sample()).unwrap();
        assert_eq!(load(&path).unwrap().len(), default_servers().len() + 1); // defaults + custom
        let mut replaced = sample();
        replaced.command = "uvx".into();
        add(&path, replaced).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), default_servers().len() + 1);
        assert_eq!(find(&path, "custom").unwrap().unwrap().command, "uvx");
    }

    #[test]
    fn remove_reports_whether_anything_was_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        add(&path, sample()).unwrap();
        assert!(remove(&path, "custom").unwrap());
        assert!(!remove(&path, "custom").unwrap());
        assert!(find(&path, "custom").unwrap().is_none());
    }

    #[test]
    fn presets_are_disabled_by_default_and_resolve_by_name() {
        let names: Vec<_> = presets().iter().map(|p| p.name).collect();
        for expected in ["brave-search", "slack", "puppeteer", "postgres", "postman", "cloudflare", "distrobox-control"] {
            assert!(names.contains(&expected), "missing preset {expected}");
        }
        assert!(preset("nonexistent").is_none());
        let spec = preset("postgres").unwrap().to_spec();
        assert_eq!(spec.command, "npx");
        assert!(!spec.enabled);
    }

    #[test]
    fn v0_1_16_catalog_expansion_added_at_least_50_new_presets_all_disabled_with_unique_names() {
        let presets = presets();
        // 7 presets existed before the v0.1.16 catalog expansion.
        assert!(presets.len() >= 57, "expected 50+ new presets on top of the original 7, got {} total", presets.len());

        let mut seen = std::collections::HashSet::new();
        for p in &presets {
            assert!(seen.insert(p.name), "duplicate preset name: {}", p.name);
            assert!(!p.to_spec().enabled, "preset {} must ship disabled", p.name);
        }
    }

    #[test]
    fn spot_check_new_preset_secret_wiring() {
        let spec = preset("notion").unwrap().to_spec();
        assert_eq!(spec.secret_env.get("NOTION_TOKEN"), Some(&"mcp:notion:NOTION_TOKEN".to_string()));

        let spec = preset("atlassian").unwrap().to_spec();
        assert_eq!(spec.command, "uvx");
        for var in ["JIRA_URL", "JIRA_USERNAME", "JIRA_API_TOKEN", "CONFLUENCE_URL", "CONFLUENCE_USERNAME", "CONFLUENCE_API_TOKEN"] {
            assert!(spec.secret_env.contains_key(var), "atlassian missing {var}");
        }

        // Servers whose credential is a positional CLI arg, not an env var,
        // must ship with no secret_env_vars (nothing to silently mis-wire).
        for name in ["redis", "sqlite", "twilio"] {
            assert!(preset(name).unwrap().to_spec().secret_env.is_empty(), "{name} should have no secret_env");
        }
    }

    #[test]
    fn secret_backed_presets_prewire_secret_env_under_the_naming_convention() {
        let spec = preset("postman").unwrap().to_spec();
        assert_eq!(spec.secret_env.get("POSTMAN_API_KEY"), Some(&"mcp:postman:POSTMAN_API_KEY".to_string()));
        assert!(spec.env.is_empty(), "the raw value must never land in the plain env map");

        let spec = preset("cloudflare").unwrap().to_spec();
        assert_eq!(spec.secret_env.get("CLOUDFLARE_API_TOKEN"), Some(&"mcp:cloudflare:CLOUDFLARE_API_TOKEN".to_string()));
    }

    #[test]
    fn presets_with_no_secret_needs_have_empty_secret_env() {
        let spec = preset("brave-search").unwrap().to_spec();
        assert!(spec.secret_env.is_empty());
    }

    #[test]
    fn gateway_mode_defaults_to_off_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_gateway.toml");
        assert!(!gateway_mode(&path).unwrap());

        set_gateway_mode(&path, true).unwrap();
        assert!(gateway_mode(&path).unwrap());

        set_gateway_mode(&path, false).unwrap();
        assert!(!gateway_mode(&path).unwrap());
    }

    #[test]
    fn gateway_server_spec_is_named_and_enabled() {
        let spec = gateway_server_spec();
        assert_eq!(spec.name, "divisi-gateway");
        assert_eq!(spec.command, "divisi-gateway");
        assert!(spec.enabled);
    }

    #[test]
    fn set_enabled_toggles_and_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        add(&path, sample()).unwrap();
        assert!(set_enabled(&path, "custom", false).unwrap());
        assert!(!find(&path, "custom").unwrap().unwrap().enabled);
        assert!(!set_enabled(&path, "ghost", true).unwrap());
    }

    #[test]
    fn sync_missing_presets_disabled_registers_every_preset_disabled_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");

        let added_first = sync_missing_presets_disabled(&path).unwrap();
        assert_eq!(added_first, presets().len());

        let servers = load(&path).unwrap();
        for preset in presets() {
            let server = servers.iter().find(|s| s.name == preset.name).unwrap_or_else(|| panic!("missing {}", preset.name));
            assert!(!server.enabled, "{} should sync in disabled", preset.name);
        }

        let added_second = sync_missing_presets_disabled(&path).unwrap();
        assert_eq!(added_second, 0, "second run should be a no-op");
    }

    #[test]
    fn sync_missing_presets_disabled_never_overwrites_an_already_registered_enabled_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        let first_preset = presets().first().unwrap().name.to_string();
        add(&path, McpServerSpec {
            name: first_preset.clone(),
            command: "custom-command".into(),
            args: vec![],
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();

        sync_missing_presets_disabled(&path).unwrap();

        let server = find(&path, &first_preset).unwrap().unwrap();
        assert!(server.enabled, "pre-existing entry must not be touched");
        assert_eq!(server.command, "custom-command");
    }
}
