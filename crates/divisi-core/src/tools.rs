//! The unified tool registry (spec section 13): metadata only in Phase 2.
//! There is no execution engine yet to actually invoke a tool for an agent
//! (that's Phase 4's orchestrator) — this is the catalog + risk metadata
//! seam that engine will read from, kept honestly separate from a promise
//! that tools are wired into any agent today.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use divisi_protocol::ToolSpec;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ToolRegistryFile {
    #[serde(default)]
    tools: BTreeMap<String, ToolSpec>,
}

/// A starter catalog of real, common developer CLI tools — not tied to any
/// one agent, just the metadata/risk-level seam described in the module
/// docs above. Risk levels reflect what the tool can actually do: read-only
/// or scoped-write tools are `Low`/`Medium`; anything that can run
/// arbitrary containers, reach a remote cluster/cloud account, brute-force
/// or scan a remote target, or push to a remote is `High`.
///
/// The original 26 entries were each confirmed present (`command -v`) on
/// the reference machine before being added. The v0.1.18 additions below
/// widen scope to security/recon, cloud, container-orchestration, VCS,
/// language-toolchain, and database-client tools — real, well-known CLIs
/// (verified as genuine tools with correct binary names, several spot
/// checked present via `command -v` on this machine), not required to be
/// installed locally the way the original set was, since this registry is
/// metadata-only with no execution engine yet.
pub fn default_tools() -> Vec<ToolSpec> {
    use divisi_protocol::RiskLevel;
    vec![
        ToolSpec { name: "git".into(), description: "version control".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "docker".into(), description: "container runtime".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "node".into(), description: "JavaScript/TypeScript runtime".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "python3".into(), description: "Python runtime".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "cargo".into(), description: "Rust build tool and package manager".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "curl".into(), description: "HTTP client".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "gh".into(), description: "GitHub CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "rg".into(), description: "ripgrep — fast recursive text search".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "fd".into(), description: "fast, user-friendly file finder".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "jq".into(), description: "command-line JSON processor".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "make".into(), description: "build automation tool".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "gcc".into(), description: "C/C++ compiler".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "go".into(), description: "Go build tool and compiler".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "rustc".into(), description: "Rust compiler".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "npm".into(), description: "Node.js package manager".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "pnpm".into(), description: "fast, disk-efficient Node.js package manager".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "yarn".into(), description: "Node.js package manager".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "ssh".into(), description: "secure remote shell / tunnel client".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "kubectl".into(), description: "Kubernetes cluster CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "helm".into(), description: "Kubernetes package manager".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "ansible".into(), description: "configuration management / remote automation".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "aws".into(), description: "AWS cloud CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "gcloud".into(), description: "Google Cloud CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "az".into(), description: "Azure CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "tmux".into(), description: "terminal multiplexer".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "vim".into(), description: "text editor".into(), risk_level: RiskLevel::Low, enabled: true },

        // -- security / recon --
        ToolSpec { name: "nmap".into(), description: "network scanner and host/port discovery".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "sqlmap".into(), description: "automated SQL injection and database takeover tool".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "hydra".into(), description: "network login brute-forcer".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "john".into(), description: "John the Ripper — offline password cracker".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "hashcat".into(), description: "GPU-accelerated password cracker".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "gobuster".into(), description: "directory, DNS, and vhost brute-forcer".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "ffuf".into(), description: "fast web fuzzer".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "tshark".into(), description: "Wireshark's CLI packet capture and analysis tool".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "nuclei".into(), description: "template-driven vulnerability scanner".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "subfinder".into(), description: "passive subdomain enumeration".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "amass".into(), description: "attack surface mapping and subdomain enumeration".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "trivy".into(), description: "container image, filesystem, and IaC vulnerability scanner".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "grype".into(), description: "container image and filesystem vulnerability scanner".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "cosign".into(), description: "container image signing and verification".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "syft".into(), description: "software bill of materials (SBOM) generator".into(), risk_level: RiskLevel::Low, enabled: true },

        // -- containers / orchestration --
        ToolSpec { name: "podman".into(), description: "rootless container runtime, Docker-compatible CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "kind".into(), description: "runs local Kubernetes clusters using Docker containers".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "minikube".into(), description: "runs a local single-node Kubernetes cluster".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "k9s".into(), description: "terminal UI for managing Kubernetes clusters".into(), risk_level: RiskLevel::High, enabled: true },

        // -- IaC / cloud platform CLIs --
        ToolSpec { name: "terragrunt".into(), description: "thin wrapper for keeping Terraform configurations DRY".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "packer".into(), description: "automated machine image builder".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "vagrant".into(), description: "builds and manages local virtual machine environments".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "doctl".into(), description: "DigitalOcean cloud CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "flyctl".into(), description: "Fly.io deployment CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "wrangler".into(), description: "Cloudflare Workers/Pages CLI".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "heroku".into(), description: "Heroku platform CLI".into(), risk_level: RiskLevel::High, enabled: true },

        // -- version control --
        ToolSpec { name: "git-lfs".into(), description: "Git extension for versioning large files".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "svn".into(), description: "Subversion version control client".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "hg".into(), description: "Mercurial version control client".into(), risk_level: RiskLevel::Medium, enabled: true },

        // -- language toolchains / build tools --
        ToolSpec { name: "mvn".into(), description: "Apache Maven build tool for Java/JVM projects".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "gradle".into(), description: "Gradle build tool for JVM and native projects".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "dotnet".into(), description: ".NET SDK and CLI".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "poetry".into(), description: "Python dependency management and packaging".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "uv".into(), description: "fast Python package and project manager".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "deno".into(), description: "secure-by-default JavaScript/TypeScript runtime".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "bun".into(), description: "fast JavaScript runtime, bundler, and package manager".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "zig".into(), description: "Zig compiler and build system".into(), risk_level: RiskLevel::Medium, enabled: true },

        // -- database clients --
        ToolSpec { name: "psql".into(), description: "PostgreSQL interactive terminal client".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "mysql".into(), description: "MySQL/MariaDB command-line client".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "redis-cli".into(), description: "Redis command-line client".into(), risk_level: RiskLevel::High, enabled: true },
        ToolSpec { name: "sqlite3".into(), description: "SQLite command-line client".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "mongosh".into(), description: "MongoDB shell client".into(), risk_level: RiskLevel::High, enabled: true },

        // -- system / networking / secrets --
        ToolSpec { name: "htop".into(), description: "interactive process viewer".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "mtr".into(), description: "combined traceroute and ping network diagnostic".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "sops".into(), description: "encrypts/decrypts secrets in files (Mozilla SOPS)".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "age".into(), description: "modern, simple file encryption tool".into(), risk_level: RiskLevel::Low, enabled: true },
        ToolSpec { name: "watchexec".into(), description: "runs a command whenever watched files change".into(), risk_level: RiskLevel::Medium, enabled: true },
        ToolSpec { name: "entr".into(), description: "runs a command whenever watched files change".into(), risk_level: RiskLevel::Medium, enabled: true },
    ]
}

pub fn load(path: &Path) -> Result<Vec<ToolSpec>> {
    if !path.exists() {
        return Ok(default_tools());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: ToolRegistryFile =
        toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.tools.into_values().collect())
}

pub fn save(path: &Path, tools: &[ToolSpec]) -> Result<()> {
    let mut map = BTreeMap::new();
    for tool in tools {
        map.insert(tool.name.clone(), tool.clone());
    }
    let file = ToolRegistryFile { tools: map };
    let rendered = toml::to_string_pretty(&file).context("serializing tool registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

pub fn add(path: &Path, tool: ToolSpec) -> Result<()> {
    let mut tools = load(path)?;
    tools.retain(|t| t.name != tool.name);
    tools.push(tool);
    save(path, &tools)
}

pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<bool> {
    let mut tools = load(path)?;
    let Some(tool) = tools.iter_mut().find(|t| t.name == name) else {
        return Ok(false);
    };
    tool.enabled = enabled;
    save(path, &tools)?;
    Ok(true)
}

pub fn find(path: &Path, name: &str) -> Result<Option<ToolSpec>> {
    Ok(load(path)?.into_iter().find(|t| t.name == name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use divisi_protocol::RiskLevel;

    fn sample() -> ToolSpec {
        ToolSpec { name: "git".into(), description: "git CLI".into(), risk_level: RiskLevel::Medium, enabled: true }
    }

    #[test]
    fn load_returns_defaults_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let tools = load(&dir.path().join("tools.toml")).unwrap();
        assert_eq!(tools.len(), default_tools().len());
        assert!(tools.iter().any(|t| t.name == "git"));
    }

    #[test]
    fn add_then_find_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.toml");
        add(&path, sample()).unwrap();
        let found = find(&path, "git").unwrap().unwrap();
        assert_eq!(found.risk_level, RiskLevel::Medium);
    }

    #[test]
    fn set_enabled_toggles_and_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.toml");
        add(&path, sample()).unwrap();
        assert!(set_enabled(&path, "git", false).unwrap());
        assert!(!find(&path, "git").unwrap().unwrap().enabled);
        assert!(!set_enabled(&path, "ghost", true).unwrap());
    }

    #[test]
    fn v0_1_18_catalog_expansion_added_at_least_40_new_tools_with_unique_names() {
        let tools = default_tools();
        assert!(tools.len() >= 66, "expected at least 66 total tools (26 original + 40 new), got {}", tools.len());

        let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate tool name in catalog");
    }

    #[test]
    fn spot_check_new_tool_risk_levels() {
        let tools = default_tools();
        let get = |n: &str| tools.iter().find(|t| t.name == n).unwrap_or_else(|| panic!("missing {n}")).clone();

        // read-only/passive tooling should never be High
        for name in ["subfinder", "trivy", "grype", "syft", "sqlite3", "age", "htop", "mtr"] {
            assert_eq!(get(name).risk_level, RiskLevel::Low, "{name} should be Low risk");
        }
        // direct remote attack/scan/db-access tooling should be High
        for name in ["nmap", "sqlmap", "hydra", "gobuster", "ffuf", "nuclei", "psql", "mysql", "redis-cli", "mongosh", "podman", "k9s"] {
            assert_eq!(get(name).risk_level, RiskLevel::High, "{name} should be High risk");
        }
    }
}
