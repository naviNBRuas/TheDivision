use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use single_core::paths::SingleDirs;
use single_core::secrets::{SecretStore, SecretTool};

// MCP gateway integration — rmcp is async, but the rest of this binary is
// synchronous (reqwest::blocking). Rather than converting everything to
// async, we create a dedicated tokio Runtime once (lazily, on first call_mcp
// invocation) and block_on just the MCP codepaths. The Runtime is reused for
// the lifetime of this single-agent process (a single CLI invocation that
// exits when the task is done), and the child MCP session spawned inside it
// dies naturally with the process — no explicit idle-eviction needed.
static MCP_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;

#[derive(Parser)]
#[command(name = "single-agent", about = "Native in-process coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// Run the agent
    Run {
        /// Provider name (must be registered in providers.toml)
        #[arg(long)]
        provider: String,

        /// Model ID to use for chat completions
        #[arg(long)]
        model: String,

        /// The user prompt. `allow_hyphen_values` is load-bearing: callers
        /// (notably `single task run`'s memory/notes context preamble)
        /// routinely prepend text starting with `---`, which clap would
        /// otherwise reject as an unexpected argument rather than accept
        /// as this flag's value.
        #[arg(long, allow_hyphen_values = true)]
        prompt: String,

        /// Working directory for file/shell operations
        #[arg(long)]
        cwd: PathBuf,

        /// Maximum agentic loop steps
        #[arg(long, default_value_t = 20)]
        max_steps: usize,
    },
}

// -- OpenAI-compatible API types --

/// Sent on every request. Reasoning models (e.g. NVIDIA's
/// deepseek-v4-flash-0731) emit an internal `reasoning_content` chain before
/// the final answer; left uncapped, that chain can run long enough to blow
/// through the 120s request timeout below, which reqwest reports as a bare
/// connection failure with no indication the real cause was an unbounded
/// generation, not a dead provider.
const DEFAULT_MAX_TOKENS: u32 = 4096;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDef>>,
    max_tokens: u32,
}

#[derive(Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    /// Reasoning models' internal chain-of-thought, returned alongside (or
    /// instead of, if `max_tokens` cuts generation off mid-thought) the
    /// final `content`. Read-only here — never constructed on our outgoing
    /// messages, so no `skip_serializing_if` is needed to keep it out of
    /// requests.
    #[serde(default, skip_serializing)]
    reasoning_content: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: FunctionCall,
    /// Provider-specific metadata riding alongside a tool call — e.g.
    /// Gemini's `extra_content.google.thought_signature`, which its
    /// OpenAI-compat endpoint REQUIRES to be echoed back verbatim on the
    /// next turn's assistant message for every function-call part, or it
    /// 400s with "Function call is missing a thought_signature" (its
    /// extended-thinking continuity protocol — confirmed live against
    /// the real API, not documented in the OpenAI-compat spec this
    /// struct otherwise follows). Captured generically (not typed to
    /// Gemini specifically) and round-tripped opaquely: this provider
    /// sends it back exactly as received, no interpretation needed, and
    /// providers that never send it (the OpenAI-compat default) leave
    /// this None and are unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_content: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ToolDef {
    #[serde(rename = "type")]
    tool_type: String,
    function: FunctionDef,
}

#[derive(Serialize)]
struct FunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Option<Message>,
}

// -- Tool argument types --

#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct RunShellArgs {
    command: String,
}

#[derive(Deserialize)]
struct CallMcpArgs {
    server: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
}

// -- Path safety --

fn canonicalize_within_cwd(cwd: &Path, rel: &str) -> Result<PathBuf> {
    if rel.starts_with('/') {
        anyhow::bail!("absolute path rejected: {rel}");
    }
    if rel.contains("..") {
        anyhow::bail!("path traversal rejected: {rel}");
    }
    let candidate = cwd.join(rel);
    let canonical = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());
    if !canonical.starts_with(cwd) {
        anyhow::bail!("path escapes working directory: {rel}");
    }
    Ok(canonical)
}

// -- Tool execution --

const FILE_CAP: usize = 50 * 1024;
const SHELL_OUTPUT_CAP: usize = 20 * 1024;
const SHELL_TIMEOUT: Duration = Duration::from_secs(30);

fn exec_read_file(cwd: &Path, args: &str) -> String {
    let parsed: ReadFileArgs = match serde_json::from_str(args) {
        Ok(p) => p,
        Err(e) => return format!("error: invalid arguments: {e}"),
    };
    let resolved = match canonicalize_within_cwd(cwd, &parsed.path) {
        Ok(p) => p,
        Err(e) => return format!("error: {e}"),
    };
    match std::fs::read(&resolved) {
        Ok(bytes) => {
            let truncated = bytes.len() > FILE_CAP;
            let content = String::from_utf8_lossy(&bytes[..bytes.len().min(FILE_CAP)]);
            if truncated {
                format!("{content}\n[truncated at {FILE_CAP} bytes]")
            } else {
                content.into_owned()
            }
        }
        Err(e) => format!("error: {e}"),
    }
}

fn exec_write_file(cwd: &Path, args: &str) -> String {
    let parsed: WriteFileArgs = match serde_json::from_str(args) {
        Ok(p) => p,
        Err(e) => return format!("error: invalid arguments: {e}"),
    };
    let resolved = match canonicalize_within_cwd(cwd, &parsed.path) {
        Ok(p) => p,
        Err(e) => return format!("error: {e}"),
    };
    if let Some(parent) = resolved.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("error: creating parent dirs: {e}");
        }
    }
    match std::fs::write(&resolved, parsed.content) {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("error: {e}"),
    }
}

fn exec_run_shell(cwd: &Path, args: &str) -> String {
    let parsed: RunShellArgs = match serde_json::from_str(args) {
        Ok(p) => p,
        Err(e) => return format!("error: invalid arguments: {e}"),
    };

    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let cwd = cwd.to_path_buf();
    let command = parsed.command.clone();
    std::thread::spawn(move || {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .output();
        let _ = tx.send(output);
    });

    match rx.recv_timeout(SHELL_TIMEOUT) {
        Ok(Ok(o)) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&o.stdout));
            if !o.stderr.is_empty() {
                combined.push_str("\n--- stderr ---\n");
                combined.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if combined.len() > SHELL_OUTPUT_CAP {
                combined.truncate(SHELL_OUTPUT_CAP);
                combined.push_str("\n[output truncated]");
            }
            combined
        }
        Ok(Err(e)) => format!("error: {e}"),
        Err(_) => "error: command timed out after 30s".to_string(),
    }
}

fn exec_call_mcp(_cwd: &Path, args: &str) -> String {
    let parsed: CallMcpArgs = match serde_json::from_str(args) {
        Ok(p) => p,
        Err(e) => return format!("error: invalid arguments: {e}"),
    };

    let rt = MCP_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime for MCP")
    });

    let server = parsed.server;
    let tool = parsed.tool;
    let arguments = parsed.arguments;

    rt.block_on(async move {
        let command = tokio::process::Command::new("single-mcp");
        let transport = match TokioChildProcess::new(command.configure(|_| {})) {
            Ok(t) => t,
            Err(e) => return format!("error: failed to spawn single-mcp: {e}"),
        };
        let session = match ().serve(transport).await {
            Ok(s) => s,
            Err(e) => return format!("error: failed to initialize MCP session: {e}"),
        };

        // single-mcp exposes exactly one relevant tool of its own,
        // `invoke_mcp` — it is NOT a transparent passthrough that lets a
        // caller list/call an arbitrary downstream server's tools directly
        // on this session (that would just be single-mcp's OWN 4 tools:
        // list_available_mcp_tools/invoke_mcp/notes_leave/notes_read).
        // Reaching a downstream server always means calling single-mcp's
        // `invoke_mcp` tool itself, passing {server, tool, arguments} as
        // ITS arguments — with `tool: null` meaning "list this server's
        // tools" and `tool: "<name>"` meaning "call it" (see
        // single-mcp/src/gateway.rs's own `invoke_mcp` handler, which this
        // mirrors exactly).
        let mut invoke_args = serde_json::Map::new();
        invoke_args.insert("server".into(), serde_json::Value::String(server.clone()));
        if let Some(ref t) = tool {
            invoke_args.insert("tool".into(), serde_json::Value::String(t.clone()));
        }
        if let Some(args) = arguments {
            invoke_args.insert("arguments".into(), serde_json::Value::Object(args));
        }

        let params = rmcp::model::CallToolRequestParams::new("invoke_mcp").with_arguments(invoke_args);
        match session.call_tool(params).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("error: serializing result: {e}")),
            Err(e) => format!("error: invoke_mcp(server={server}, tool={tool:?}): {e}"),
        }
    })
}

// -- Tool definitions for the API --

fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            tool_type: "function".into(),
            function: FunctionDef {
                name: "read_file".into(),
                description: "Read a file's contents. Path is relative to --cwd.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path relative to the working directory"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: FunctionDef {
                name: "write_file".into(),
                description: "Write content to a file. Path is relative to --cwd. Creates parent directories as needed.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path relative to the working directory"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: FunctionDef {
                name: "run_shell".into(),
                description: "Run a shell command with sh -c. Working directory is --cwd. 30s timeout.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            tool_type: "function".into(),
            function: FunctionDef {
                name: "call_mcp".into(),
                description: "Call an MCP tool via SingleCLI's MCP gateway (single-mcp). \
                    Use server + tool: null to list a server's available tools, \
                    or server + tool + arguments to call a specific tool."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": {
                            "type": "string",
                            "description": "Registered MCP server name (from single mcp list)"
                        },
                        "tool": {
                            "type": "string",
                            "description": "Tool name to call; omit or set null to list this server's tools"
                        },
                        "arguments": {
                            "type": "object",
                            "description": "Arguments for the tool call, if any"
                        }
                    },
                    "required": ["server"]
                }),
            },
        },
    ]
}

// -- Agentic loop --

fn run_agent_loop(
    client: &Client,
    api_url: &str,
    api_key: &str,
    model: &str,
    cwd: &Path,
    prompt: &str,
    max_steps: usize,
) -> Result<()> {
    let system_msg = format!(
        "You are a coding agent operating in the directory: {}. \
         Use the available tools to read files, write files, and run shell commands \
         to accomplish the user's task. Always work relative to the given directory.",
        cwd.display()
    );

    let mut messages: Vec<Message> = vec![
        Message {
            role: "system".into(),
            content: Some(system_msg),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        Message {
            role: "user".into(),
            content: Some(prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ];

    for _ in 0..max_steps {
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.clone(),
            tools: Some(tool_definitions()),
            max_tokens: DEFAULT_MAX_TOKENS,
        };

        let response = client
            .post(api_url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(120))
            .json(&request)
            .send()
            .context("HTTP request to provider failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .unwrap_or_else(|_| "<could not read error body>".into());
            eprintln!("Provider error ({status}): {body}");
            std::process::exit(1);
        }

        let chat_response: ChatResponse = response
            .json()
            .context("failed to parse chat completions response")?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .context("provider returned no choices")?;

        let assistant_msg = match choice.message {
            Some(m) => m,
            None => {
                eprintln!("provider returned empty message");
                std::process::exit(1);
            }
        };

        let tool_calls = assistant_msg.tool_calls.unwrap_or_default();
        if tool_calls.is_empty() {
            match assistant_msg.content {
                Some(content) => println!("{content}"),
                None => match assistant_msg.reasoning_content {
                    // A reasoning model cut off mid-thought (hit
                    // max_tokens before emitting a final answer) — surface
                    // what it had reasoned so far rather than nothing.
                    Some(reasoning) => println!("{reasoning}"),
                    None => eprintln!(
                        "provider returned no content and no reasoning_content \
                         (finished with neither an answer nor a tool call)"
                    ),
                },
            }
            return Ok(());
        }

        // Append the assistant message with tool calls. content: null here
        // is valid per the OpenAI spec, but at least one provider's
        // "OpenAI-compatible" shim (Cloudflare Workers AI, confirmed live)
        // rejects a null content field outright even when tool_calls is
        // present — an empty string round-trips through every provider
        // tested (OpenAI-spec-compliant ones treat "" the same as null
        // here) without losing anything real, since content is genuinely
        // absent in this turn either way.
        messages.push(Message {
            role: "assistant".into(),
            content: Some(assistant_msg.content.unwrap_or_default()),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            reasoning_content: None,
        });

        // Execute each tool call and append results
        for tc in &tool_calls {
            let result = match tc.function.name.as_str() {
                "read_file" => exec_read_file(cwd, &tc.function.arguments),
                "write_file" => exec_write_file(cwd, &tc.function.arguments),
                "run_shell" => exec_run_shell(cwd, &tc.function.arguments),
                "call_mcp" => exec_call_mcp(cwd, &tc.function.arguments),
                other => format!("error: unknown tool: {other}"),
            };
            messages.push(Message {
                role: "tool".into(),
                content: Some(result),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                reasoning_content: None,
            });
        }
    }

    eprintln!("reached maximum steps ({max_steps}) without a final answer");
    std::process::exit(1);
}

// -- Main --

fn main() -> Result<()> {
    let cli = Cli::parse();
    let Command::Run {
        provider,
        model,
        prompt,
        cwd,
        max_steps,
    } = cli.command;

    if !cwd.is_dir() {
        eprintln!("error: --cwd does not exist or is not a directory: {}", cwd.display());
        std::process::exit(1);
    }

    // Resolve the provider via the SingleCLI registry
    let dirs = SingleDirs::discover().context("resolving SingleCLI config directory")?;
    let provider_spec = single_core::providers::find(&dirs.providers_registry_file(), &provider)
        .context("reading providers registry")?
        .unwrap_or_else(|| {
            eprintln!("error: provider '{provider}' is not registered in providers.toml");
            std::process::exit(1);
        });

    // Get the API key from the OS keychain
    let store = SecretTool;
    let api_key = SecretStore::get(&store, &provider_spec.secret_name)
        .context("accessing secret store")?
        .unwrap_or_else(|| {
            eprintln!(
                "error: no API key stored for provider '{provider}' (secret name: {}). Run `single provider set-key` first.",
                provider_spec.secret_name
            );
            std::process::exit(1);
        });

    // Build the chat completions URL. Some registered providers' base
    // URLs carry a trailing slash (e.g. Gemini's OpenAI-compat endpoint,
    // "https://generativelanguage.googleapis.com/v1beta/openai/") --
    // joining that naively produces a double slash before
    // "chat/completions", which several providers' API gateways 404 on
    // (confirmed against Gemini's real endpoint) even though the
    // double-slash URL looks harmless. Strip it so this can't bite any
    // current or future provider registration.
    let base_url = provider_spec
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/');
    let api_url = format!("{base_url}/chat/completions");

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("building HTTP client")?;

    run_agent_loop(&client, &api_url, &api_key, &model, &cwd, &prompt, max_steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        let cwd = PathBuf::from("/home/user/project");
        assert!(canonicalize_within_cwd(&cwd, "../etc/passwd").is_err());
        assert!(canonicalize_within_cwd(&cwd, "subdir/../../etc/passwd").is_err());
    }

    #[test]
    fn rejects_absolute_path_outside_cwd() {
        let cwd = PathBuf::from("/home/user/project");
        assert!(canonicalize_within_cwd(&cwd, "/etc/passwd").is_err());
        assert!(canonicalize_within_cwd(&cwd, "/tmp/evil.txt").is_err());
    }

    #[test]
    fn accepts_valid_relative_path_within_cwd() {
        let cwd = PathBuf::from("/home/user/project");
        let result = canonicalize_within_cwd(&cwd, "src/main.rs");
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with(&cwd));
    }

    #[test]
    fn accepts_nested_relative_path_within_cwd() {
        let cwd = PathBuf::from("/home/user/project");
        let result = canonicalize_within_cwd(&cwd, "a/b/c/d.txt");
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with(&cwd));
    }

    #[test]
    fn tool_definitions_includes_call_mcp_with_correct_schema() {
        let defs = tool_definitions();
        let call_mcp = defs.iter().find(|d| d.function.name == "call_mcp");
        assert!(call_mcp.is_some(), "call_mcp tool definition must exist");

        let params = &call_mcp.unwrap().function.parameters;
        let required = params["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("server")));
        let properties = params["properties"].as_object().unwrap();
        assert!(properties.contains_key("server"));
        assert!(properties.contains_key("tool"));
        assert!(properties.contains_key("arguments"));
    }

    #[test]
    fn call_mcp_args_parses_with_only_server() {
        let args = r#"{"server": "context7"}"#;
        let parsed: CallMcpArgs = serde_json::from_str(args).unwrap();
        assert_eq!(parsed.server, "context7");
        assert_eq!(parsed.tool, None);
        assert_eq!(parsed.arguments, None);
    }

    #[test]
    fn call_mcp_args_parses_full_invocation() {
        let args = r#"{"server": "context7", "tool": "search", "arguments": {"query": "foo"}}"#;
        let parsed: CallMcpArgs = serde_json::from_str(args).unwrap();
        assert_eq!(parsed.server, "context7");
        assert_eq!(parsed.tool.as_deref(), Some("search"));
        assert!(parsed.arguments.is_some());
    }
}
