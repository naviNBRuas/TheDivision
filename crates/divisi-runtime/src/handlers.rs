use crate::context::Context;
use crate::{bootstrap, doctor, integrations, memory};
use anyhow::Context as _;
use divisi_agent_sdk::adapters::for_agent_with_custom;
use divisi_agent_sdk::Discovery;
use divisi_core::registry::AgentDefinition;
use divisi_protocol::{
    AccountingQuery as ProtoAccountingQuery, AgentInfo, McpServerInfo,
    Request, Response, ResponseData, RuntimeStatus,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub fn handle(ctx: &Context, request: Request) -> Response {
    handle_with_registry(ctx, request, &crate::registry::TaskRegistry::default())
}

/// Same as `handle`, but with an explicit `TaskRegistry` — used by the real
/// daemon (`server.rs`), which creates one registry at startup and shares
/// it (cloned — cheap, it's an `Arc`) across every connection so a
/// `TaskCancel` sent on one connection can find a task started with
/// `background: true` on another. `handle`'s throwaway per-call registry
/// is only reachable from `client::send`'s no-daemon-running fallback,
/// where there's no persistent process for a later `TaskCancel` to even
/// reach anyway — a `background: true` run there still starts, but has
/// nothing keeping it alive once the one-shot CLI invocation exits.
pub fn handle_with_registry(
    ctx: &Context,
    request: Request,
    registry: &crate::registry::TaskRegistry,
) -> Response {
    match dispatch(ctx, request, registry) {
        Ok(data) => Response::Ok { data },
        Err(e) => Response::Error {
            message: format!("{e:#}"),
        },
    }
}

/// Process-wide "a `doctor` run is in flight" flag.
static DOCTOR_RUNNING: AtomicBool = AtomicBool::new(false);

/// RAII guard for `DOCTOR_RUNNING`: `acquire` fails if a run is already
/// active, and `Drop` clears the flag on every exit path (normal return
/// or panic-unwind through `dispatch`), so a panicking `doctor::run`
/// can't wedge the daemon into permanently refusing `doctor`.
#[derive(Debug)]
struct DoctorGuard;

impl DoctorGuard {
    fn acquire() -> anyhow::Result<Self> {
        if DOCTOR_RUNNING.swap(true, Ordering::AcqRel) {
            anyhow::bail!("doctor already running; wait for the in-flight run to finish");
        }
        Ok(DoctorGuard)
    }
}

impl Drop for DoctorGuard {
    fn drop(&mut self) {
        DOCTOR_RUNNING.store(false, Ordering::Release);
    }
}

fn dispatch(
    ctx: &Context,
    request: Request,
    registry: &crate::registry::TaskRegistry,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::Status => Ok(ResponseData::Status(status(ctx))),
        Request::Doctor { fix } => {
            // `doctor` probes every registered agent; two runs at once
            // double the subprocess fan-out on the daemon for no benefit
            // (confirmed cause of a compounded RSS spike). Serve the first,
            // reject the rest until it finishes.
            let _guard = DoctorGuard::acquire()?;
            if fix {
                let conn = coordinator_db(ctx)?;
                crate::pool::ensure_pool_schema(&conn)?;
                if let Err(e) = crate::self_heal::run_pass(ctx, &conn, None) {
                    tracing::warn!(error = %e, "doctor --fix: self-heal pass failed");
                }
            }
            Ok(ResponseData::Doctor(doctor::run(ctx)))
        }
        // Actual process exit happens in server.rs after this response is
        // flushed to the client — see its handle_connection.
        Request::Shutdown => {
            // E28 spec §10: a clean stop, distinct from a crash — mark
            // active goals `Paused` so `resume_interrupted` (not the
            // crash-oriented reconcile) picks them back up next start.
            // Best-effort: this must never block the daemon from exiting.
            if let Ok(conn) = coordinator_db(ctx) {
                let _ = crate::coordinator::pause_active_goals(&conn);
            }
            Ok(ResponseData::Empty)
        }
        Request::AgentList => Ok(ResponseData::Agents(agent_list_info(ctx))),
        Request::AgentInspect { name } => {
            let def = ctx
                .find_agent(&name)
                .ok_or_else(|| anyhow::anyhow!("no such agent: {name}"))?;
            Ok(ResponseData::Agent(to_agent_info(def, ctx)))
        }
        Request::McpList => {
            let servers = divisi_core::mcp::load(&ctx.dirs.mcp_registry_file())?;
            let enabled_agents: Vec<String> = ctx.registry.iter().map(|a| a.name.clone()).collect();
            let infos = servers
                .into_iter()
                .map(|s| McpServerInfo {
                    name: s.name,
                    command: format!("{} {}", s.command, s.args.join(" "))
                        .trim()
                        .to_string(),
                    enabled: s.enabled,
                    synced_to: enabled_agents.clone(),
                })
                .collect();
            Ok(ResponseData::McpServers(infos))
        }
        Request::McpAdd { server } => {
            divisi_core::mcp::add(&ctx.dirs.mcp_registry_file(), server)?;
            Ok(ResponseData::Empty)
        }
        Request::McpRemove { name } => {
            if !divisi_core::mcp::remove(&ctx.dirs.mcp_registry_file(), &name)? {
                anyhow::bail!("no such mcp server: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::McpEnable { name } => {
            if !divisi_core::mcp::set_enabled(&ctx.dirs.mcp_registry_file(), &name, true)? {
                anyhow::bail!("no such mcp server: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::McpDisable { name } => {
            if !divisi_core::mcp::set_enabled(&ctx.dirs.mcp_registry_file(), &name, false)? {
                anyhow::bail!("no such mcp server: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::McpInspect { name } => {
            let server = divisi_core::mcp::find(&ctx.dirs.mcp_registry_file(), &name)?
                .ok_or_else(|| anyhow::anyhow!("no such mcp server: {name}"))?;
            Ok(ResponseData::McpServer(server))
        }
        Request::McpPresetList => {
            let presets = divisi_core::mcp::presets()
                .into_iter()
                .map(|p| divisi_protocol::McpPresetInfo {
                    name: p.name.to_string(),
                    command: p.command.to_string(),
                    args: p.args.iter().map(|s| s.to_string()).collect(),
                })
                .collect();
            Ok(ResponseData::McpPresets(presets))
        }
        Request::McpAddPreset { name } => {
            let preset = divisi_core::mcp::preset(&name).ok_or_else(|| {
                anyhow::anyhow!("no such preset: {name} (see `divisi mcp presets`)")
            })?;
            divisi_core::mcp::add(&ctx.dirs.mcp_registry_file(), preset.to_spec())?;
            Ok(ResponseData::Empty)
        }
        Request::McpGatewaySetEnabled { enabled } => {
            divisi_core::mcp::set_gateway_mode(&ctx.dirs.mcp_gateway_file(), enabled)?;
            Ok(ResponseData::Empty)
        }
        Request::McpGatewayStatus => Ok(ResponseData::McpGatewayMode(
            divisi_core::mcp::gateway_mode(&ctx.dirs.mcp_gateway_file())?,
        )),
        Request::LspList => Ok(ResponseData::LspServers(divisi_core::lsp::load(
            &ctx.dirs.lsp_registry_file(),
        )?)),
        Request::LspAdd { server } => {
            divisi_core::lsp::add(&ctx.dirs.lsp_registry_file(), server)?;
            Ok(ResponseData::Empty)
        }
        Request::LspRemove { name } => {
            if !divisi_core::lsp::remove(&ctx.dirs.lsp_registry_file(), &name)? {
                anyhow::bail!("no such lsp server: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::LspEnable { name } => {
            if !divisi_core::lsp::set_enabled(&ctx.dirs.lsp_registry_file(), &name, true)? {
                anyhow::bail!("no such lsp server: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::LspDisable { name } => {
            if !divisi_core::lsp::set_enabled(&ctx.dirs.lsp_registry_file(), &name, false)? {
                anyhow::bail!("no such lsp server: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::LspInspect { name } => {
            let server = divisi_core::lsp::find(&ctx.dirs.lsp_registry_file(), &name)?
                .ok_or_else(|| anyhow::anyhow!("no such lsp server: {name}"))?;
            Ok(ResponseData::LspServer(server))
        }
        Request::LspPresetList => {
            let presets = divisi_core::lsp::presets()
                .into_iter()
                .map(|p| divisi_protocol::LspPresetInfo {
                    name: p.name.to_string(),
                    command: p.command.to_string(),
                    args: p.args.iter().map(|s| s.to_string()).collect(),
                    extensions: p.extensions.iter().map(|s| s.to_string()).collect(),
                })
                .collect();
            Ok(ResponseData::LspPresets(presets))
        }
        Request::LspAddPreset { name } => {
            let preset = divisi_core::lsp::preset(&name).ok_or_else(|| {
                anyhow::anyhow!("no such preset: {name} (see `divisi lsp presets`)")
            })?;
            divisi_core::lsp::add(&ctx.dirs.lsp_registry_file(), preset.to_spec())?;
            Ok(ResponseData::Empty)
        }
        Request::ToolList => Ok(ResponseData::Tools(divisi_core::tools::load(
            &ctx.dirs.tools_registry_file(),
        )?)),
        Request::ToolAdd { tool } => {
            divisi_core::tools::add(&ctx.dirs.tools_registry_file(), tool)?;
            Ok(ResponseData::Empty)
        }
        Request::ToolInspect { name } => {
            let tool = divisi_core::tools::find(&ctx.dirs.tools_registry_file(), &name)?
                .ok_or_else(|| anyhow::anyhow!("no such tool: {name}"))?;
            Ok(ResponseData::Tool(tool))
        }
        Request::ToolEnable { name } => {
            if !divisi_core::tools::set_enabled(&ctx.dirs.tools_registry_file(), &name, true)? {
                anyhow::bail!("no such tool: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::ToolDisable { name } => {
            if !divisi_core::tools::set_enabled(&ctx.dirs.tools_registry_file(), &name, false)? {
                anyhow::bail!("no such tool: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::SecretList => {
            let store = divisi_core::secrets::SecretTool;
            Ok(ResponseData::SecretNames(
                divisi_core::secrets::SecretStore::list(&store)?,
            ))
        }
        Request::SecretSet { name, value } => {
            let store = divisi_core::secrets::SecretTool;
            divisi_core::secrets::SecretStore::set(&store, &name, &value)?;
            Ok(ResponseData::Empty)
        }
        Request::SecretGet { name } => {
            let store = divisi_core::secrets::SecretTool;
            Ok(ResponseData::SecretValue(
                divisi_core::secrets::SecretStore::get(&store, &name)?,
            ))
        }
        Request::SecretDelete { name } => {
            let store = divisi_core::secrets::SecretTool;
            if !divisi_core::secrets::SecretStore::delete(&store, &name)? {
                anyhow::bail!("no such secret: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::SecretPromoteAlias { alias, name } => {
            let conn = coordinator_db(ctx)?;
            divisi_core::redact::ensure_schema(&conn)?;
            let redact_store = divisi_core::redact::RedactStore { conn: &conn };
            let secret_store = divisi_core::secrets::SecretTool;
            let value = divisi_core::redact::take_alias_value(&redact_store, &secret_store, &alias)?;
            divisi_core::secrets::SecretStore::set(&secret_store, &name, &value)?;
            Ok(ResponseData::Empty)
        }
        Request::SkillList => Ok(ResponseData::Skills(divisi_core::skills::list(
            &ctx.dirs.skills_dir(),
        )?)),
        Request::SkillInstall { name, source_path } => {
            divisi_core::skills::install(
                &ctx.dirs.skills_dir(),
                &name,
                std::path::Path::new(&source_path),
            )?;
            Ok(ResponseData::Empty)
        }
        Request::SkillRemove { name } => {
            if !divisi_core::skills::remove(&ctx.dirs.skills_dir(), &name)? {
                anyhow::bail!("no such skill: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::SkillInspect { name } => {
            let contents = divisi_core::skills::inspect(&ctx.dirs.skills_dir(), &name)?
                .ok_or_else(|| anyhow::anyhow!("no such skill: {name}"))?;
            Ok(ResponseData::SkillContents(contents))
        }
        Request::SkillSyncClaude { name } => {
            let real_home = integrations::home_dir()?;
            let home = divisi_core::agent_home::ensure_bootstrapped(
                &ctx.dirs.homes_dir(),
                &real_home,
                "claude",
            )?;
            let claude_skills_dir = home.join(".claude").join("skills");
            let dest = divisi_core::skills::sync_to_claude(
                &ctx.dirs.skills_dir(),
                &claude_skills_dir,
                &name,
            )?;
            Ok(ResponseData::SkillSynced {
                path: dest.display().to_string(),
            })
        }
        Request::SkillStarterList => {
            let starters = divisi_core::skills::starter_set()
                .into_iter()
                .map(|s| divisi_protocol::SkillStarterInfo {
                    name: s.name.to_string(),
                    description: s.description.to_string(),
                })
                .collect();
            Ok(ResponseData::SkillStarters(starters))
        }
        Request::SkillInstallStarter { name } => {
            divisi_core::skills::install_starter(&ctx.dirs.skills_dir(), &name)?;
            Ok(ResponseData::Empty)
        }
        Request::MemoryStore {
            scope,
            source,
            project,
            agent,
            task,
            title,
            content,
            confidence,
            expires_in_seconds,
        } => {
            let conn = memory_db(ctx)?;
            let id = memory::store(
                &conn,
                memory::NewMemory {
                    scope,
                    source,
                    project: project.clone(),
                    agent,
                    task,
                    title: title.clone(),
                    content: content.clone(),
                    confidence,
                    expires_in_seconds,
                },
            )?;
            // Best-effort: index for semantic search if both an embeddings
            // key and DIVISI_QDRANT_URL are configured. Never fails the
            // write itself — see embeddings.rs's module docs.
            if let Some(url) = crate::qdrant_backend::resolve_url() {
                if let Ok(vector) = crate::embeddings::embed(&format!("{title}\n{content}")) {
                    let payload = serde_json::json!({ "memory_id": id, "project": project });
                    let _ = crate::qdrant_backend::upsert_point(
                        &url,
                        "single_memory",
                        id as u64,
                        &vector,
                        payload,
                    );
                }
            }
            Ok(ResponseData::MemoryId(id))
        }
        Request::MemorySearch {
            query,
            scope,
            project,
        } => {
            let conn = memory_db(ctx)?;
            let entries = memory::search(&conn, &query, scope, project.as_deref())?;
            Ok(ResponseData::MemoryEntries(entries))
        }
        Request::MemorySearchSemantic {
            query,
            scope,
            project,
            limit,
        } => {
            let conn = memory_db(ctx)?;
            let semantic: anyhow::Result<Vec<divisi_protocol::MemoryEntry>> = (|| {
                let url =
                    crate::qdrant_backend::resolve_url().context("DIVISI_QDRANT_URL is not set")?;
                let vector = crate::embeddings::embed(&query)?;
                let hits = crate::qdrant_backend::search(&url, "single_memory", &vector, limit)?;
                let mut entries = Vec::new();
                for hit in hits {
                    let Some(memory_id) = hit.payload.get("memory_id").and_then(|v| v.as_i64())
                    else {
                        continue;
                    };
                    if let Some(entry) = memory::get(&conn, memory_id)? {
                        if project.is_none() || entry.project == project {
                            entries.push(entry);
                        }
                    }
                }
                Ok(entries)
            })();
            match semantic {
                Ok(entries) => Ok(ResponseData::MemoryEntries(entries)),
                Err(e) => {
                    eprintln!("note: semantic memory search unavailable ({e:#}) — falling back to substring search");
                    Ok(ResponseData::MemoryEntries(memory::search(
                        &conn,
                        &query,
                        scope,
                        project.as_deref(),
                    )?))
                }
            }
        }
        Request::MemoryGet { id } => {
            let conn = memory_db(ctx)?;
            let entry =
                memory::get(&conn, id)?.ok_or_else(|| anyhow::anyhow!("no memory with id {id}"))?;
            Ok(ResponseData::MemoryEntry(entry))
        }
        Request::MemoryDelete { id } => {
            let conn = memory_db(ctx)?;
            if !memory::delete(&conn, id)? {
                anyhow::bail!("no memory with id {id}");
            }
            Ok(ResponseData::Empty)
        }
        Request::MemoryList { scope } => {
            let conn = memory_db(ctx)?;
            Ok(ResponseData::MemoryEntries(memory::list(&conn, scope)?))
        }
        Request::NoteLeave {
            project,
            from_agent,
            to_agent,
            topic,
            content,
        } => {
            let conn = notes_db(ctx)?;
            let id = divisi_core::notes::leave(
                &conn,
                project,
                &from_agent,
                to_agent.as_deref(),
                &topic,
                &content,
            )?;
            Ok(ResponseData::NoteId(id))
        }
        Request::NoteInbox {
            project,
            to_agent,
            unread_only,
        } => {
            let conn = notes_db(ctx)?;
            Ok(ResponseData::Notes(divisi_core::notes::inbox(
                &conn,
                project.as_deref(),
                &to_agent,
                unread_only,
            )?))
        }
        Request::NoteMarkRead { id } => {
            let conn = notes_db(ctx)?;
            if !divisi_core::notes::mark_read(&conn, id)? {
                anyhow::bail!("no unread note with id {id}");
            }
            Ok(ResponseData::Empty)
        }
        Request::DocumentIngest {
            path,
            project,
            title,
        } => {
            let conn = documents_db(ctx)?;
            let doc = crate::documents::ingest(
                &conn,
                &ctx.dirs.documents_dir(),
                std::path::Path::new(&path),
                project,
                title,
            )?;
            Ok(ResponseData::Document(to_document_info(doc)))
        }
        Request::DocumentList { project } => {
            let conn = documents_db(ctx)?;
            let docs = crate::documents::list(&conn, project.as_deref())?
                .into_iter()
                .map(to_document_info)
                .collect();
            Ok(ResponseData::Documents(docs))
        }
        Request::DocumentGet { id } => {
            let conn = documents_db(ctx)?;
            let doc = crate::documents::get(&conn, id)?
                .ok_or_else(|| anyhow::anyhow!("no document with id {id}"))?;
            Ok(ResponseData::Document(to_document_info(doc)))
        }
        Request::ContextShow { cwd } => Ok(ResponseData::Context(
            divisi_core::project_context::resolve(std::path::Path::new(&cwd)),
        )),
        Request::TaskRun {
            description,
            agent,
            cwd,
            use_worktree,
            account,
            real_home,
            no_memory_context,
            timeout_secs,
            background,
            allow_fallback,
            usage_json,
        } => {
            // redaction scope matches `pool_agent::run_as_task`'s own
            // `session_key` (= `account`), so `resolve()` at the dispatch
            // boundary finds the same aliases this scan just wrote.
            let description = {
                let conn = task_db(ctx)?;
                divisi_core::redact::ensure_schema(&conn)?;
                let redact_store = divisi_core::redact::RedactStore { conn: &conn };
                let secret_store = divisi_core::secrets::SecretTool;
                let session_id = account.as_deref().unwrap_or("no-session");
                let (redacted, _aliases) =
                    divisi_core::redact::scan_and_replace(&redact_store, &secret_store, session_id, &description)?;
                redacted
            };
            if background {
                let record = crate::task::run_background(
                    ctx,
                    crate::task::OwnedRunTaskOptions {
                        description,
                        agent,
                        cwd: std::path::PathBuf::from(cwd),
                        use_worktree,
                        account,
                        real_home,
                        no_memory_context,
                        timeout: std::time::Duration::from_secs(timeout_secs),
                        allow_fallback,
                        usage_json,
                        require_structured_output: false,
                        pool_agentic: true,
                    },
                    registry.clone(),
                )?;
                return Ok(ResponseData::Task(record));
            }
            let conn = task_db(ctx)?;
            let record = crate::task::run(
                &conn,
                ctx,
                crate::task::RunTaskOptions {
                    description: &description,
                    agent: &agent,
                    cwd: std::path::Path::new(&cwd),
                    use_worktree,
                    account: account.as_deref(),
                    real_home,
                    no_memory_context,
                    timeout: std::time::Duration::from_secs(timeout_secs),
                    allow_fallback,
                    usage_json,
                    require_structured_output: false,
                    pool_agentic: true,
                },
            )?;
            Ok(ResponseData::Task(record))
        }
        Request::TaskList => {
            let conn = task_db(ctx)?;
            Ok(ResponseData::Tasks(crate::task::list(&conn)?))
        }
        Request::WorkspaceList => {
            let conn = task_db(ctx)?;
            Ok(ResponseData::Workspaces(crate::task::list_workspaces(&conn)?))
        }
        Request::TaskInspect { id } => {
            let conn = task_db(ctx)?;
            let record = crate::task::get(&conn, id)?
                .ok_or_else(|| anyhow::anyhow!("no task with id {id}"))?;
            Ok(ResponseData::Task(record))
        }
        Request::TaskCancel { id, force } => {
            if !registry.cancel(id) {
                if force {
                    let conn = task_db(ctx)?;
                    crate::task::force_fail(&conn, id, "force-cancelled (no live process)")?;
                    return Ok(ResponseData::Empty);
                }
                anyhow::bail!(
                    "task #{id} isn't currently running in the background — nothing to cancel (pass --force to clear a stuck row)"
                );
            }
            Ok(ResponseData::Empty)
        }
        Request::TaskCleanup { id, force } => {
            let conn = task_db(ctx)?;
            crate::task::cleanup(&conn, ctx, id, force)?;
            Ok(ResponseData::Empty)
        }
        Request::WorktreeMergePreview { task_id } => {
            let conn = task_db(ctx)?;
            let task = crate::task::get(&conn, task_id)?
                .ok_or_else(|| anyhow::anyhow!("no such task: #{task_id}"))?;
            if task.status == divisi_protocol::TaskStatus::Running
                || task.status == divisi_protocol::TaskStatus::Created
            {
                anyhow::bail!(
                    "task #{task_id} is still {}; wait for it to finish first",
                    crate::task::status_as_str(task.status)
                );
            }
            if task.worktree_path.is_none() {
                anyhow::bail!(
                    "task #{task_id} did not run in a worktree (use_worktree was not set); nothing to merge"
                );
            }
            let repo_root = divisi_core::project_context::resolve(std::path::Path::new(&task.cwd))
                .repo_root
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "task #{task_id}'s original cwd '{}' is not inside a git repository",
                        task.cwd
                    )
                })?;
            let branch = format!("divisi/task-{task_id}");
            let diff_output = divisi_core::worktree::diff(std::path::Path::new(&repo_root), &branch)?;
            Ok(ResponseData::WorktreeDiff(diff_output))
        }
        Request::WorktreeMergeApply { task_id } => {
            let conn = task_db(ctx)?;
            let task = crate::task::get(&conn, task_id)?
                .ok_or_else(|| anyhow::anyhow!("no such task: #{task_id}"))?;
            if task.status == divisi_protocol::TaskStatus::Running
                || task.status == divisi_protocol::TaskStatus::Created
            {
                anyhow::bail!(
                    "task #{task_id} is still {}; wait for it to finish first",
                    crate::task::status_as_str(task.status)
                );
            }
            if task.worktree_path.is_none() {
                anyhow::bail!(
                    "task #{task_id} did not run in a worktree (use_worktree was not set); nothing to merge"
                );
            }
            let repo_root = divisi_core::project_context::resolve(std::path::Path::new(&task.cwd))
                .repo_root
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "task #{task_id}'s original cwd '{}' is not inside a git repository",
                        task.cwd
                    )
                })?;
            let branch = format!("divisi/task-{task_id}");
            let output = divisi_core::worktree::merge(std::path::Path::new(&repo_root), &branch)?;
            Ok(ResponseData::WorktreeMerged(divisi_protocol::WorktreeMergeResult {
                task_id,
                branch,
                output,
            }))
        }
        Request::Orchestrate {
            goal,
            agents,
            cwd,
            use_worktree,
            real_home,
            timeout_secs,
        } => {
            let conn = task_db(ctx)?;
            let records = crate::orchestrate::run(
                &conn,
                ctx,
                crate::orchestrate::OrchestrateOptions {
                    goal: &goal,
                    agents: &agents,
                    cwd: std::path::Path::new(&cwd),
                    use_worktree,
                    real_home,
                    timeout: std::time::Duration::from_secs(timeout_secs),
                },
            )?;
            Ok(ResponseData::OrchestrateResult(records))
        }
        Request::OrchestrateParallel {
            tasks,
            cwd,
            real_home,
            timeout_secs,
            background,
            orchestrator,
            goal,
            candidate_agents,
        } => {
            if orchestrator != divisi_protocol::OrchestratorMode::Fixed {
                let goal = goal.context("non-fixed orchestration requires a goal")?;
                let cwd_buf = std::path::PathBuf::from(cwd);
                if background {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = crate::orchestrate_graph::plan_and_run(
                            &ctx,
                            orchestrator,
                            &goal,
                            &candidate_agents,
                            &cwd_buf,
                            real_home,
                            std::time::Duration::from_secs(timeout_secs),
                        );
                    });
                    return Ok(ResponseData::OrchestrateResult(Vec::new()));
                }
                let records = crate::orchestrate_graph::plan_and_run(
                    ctx,
                    orchestrator,
                    &goal,
                    &candidate_agents,
                    &cwd_buf,
                    real_home,
                    std::time::Duration::from_secs(timeout_secs),
                )?;
                return Ok(ResponseData::OrchestrateResult(records));
            }
            let task_pairs: Vec<(String, String)> = tasks
                .into_iter()
                .map(|t| (t.agent, t.description))
                .collect();
            if background {
                // run_parallel is itself a blocking join() over every
                // sub-task's own thread — unlike TaskRun::background,
                // there's no pre-created row to hand back immediately
                // here (each sub-task creates its own inside its thread),
                // so the honest contract is: this returns an empty batch
                // right away, and the real per-task records show up in
                // `TaskList`/`TaskInspect` as each one starts and finishes.
                let ctx = ctx.clone();
                let cwd_buf = std::path::PathBuf::from(cwd);
                std::thread::spawn(move || {
                    let _ = crate::orchestrate::run_parallel(
                        &ctx,
                        crate::orchestrate::ParallelOrchestrateOptions {
                            tasks: &task_pairs,
                            cwd: &cwd_buf,
                            real_home,
                            timeout: std::time::Duration::from_secs(timeout_secs),
                        },
                    );
                });
                return Ok(ResponseData::OrchestrateResult(Vec::new()));
            }
            let records = crate::orchestrate::run_parallel(
                ctx,
                crate::orchestrate::ParallelOrchestrateOptions {
                    tasks: &task_pairs,
                    cwd: std::path::Path::new(&cwd),
                    real_home,
                    timeout: std::time::Duration::from_secs(timeout_secs),
                },
            )?;
            Ok(ResponseData::OrchestrateResult(records))
        }
        Request::OrchestrateGraph {
            nodes,
            cwd,
            real_home,
            timeout_secs,
            background,
            orchestrator,
            goal,
            candidate_agents,
        } => {
            if orchestrator != divisi_protocol::OrchestratorMode::Fixed {
                let goal = goal.context("non-fixed orchestration requires a goal")?;
                let cwd_buf = std::path::PathBuf::from(cwd);
                if background {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = crate::orchestrate_graph::plan_and_run(
                            &ctx,
                            orchestrator,
                            &goal,
                            &candidate_agents,
                            &cwd_buf,
                            real_home,
                            std::time::Duration::from_secs(timeout_secs),
                        );
                    });
                    return Ok(ResponseData::OrchestrateGraphResult(Vec::new()));
                }
                let records = crate::orchestrate_graph::plan_and_run(
                    ctx,
                    orchestrator,
                    &goal,
                    &candidate_agents,
                    &cwd_buf,
                    real_home,
                    std::time::Duration::from_secs(timeout_secs),
                )?;
                return Ok(ResponseData::OrchestrateGraphResult(records));
            }
            if background {
                let ctx = ctx.clone();
                let cwd_buf = std::path::PathBuf::from(cwd);
                std::thread::spawn(move || {
                    let _ = crate::orchestrate_graph::run(
                        &ctx,
                        crate::orchestrate_graph::GraphOrchestrateOptions {
                            nodes: &nodes,
                            cwd: &cwd_buf,
                            real_home,
                            timeout: std::time::Duration::from_secs(timeout_secs),
                        },
                    );
                });
                return Ok(ResponseData::OrchestrateGraphResult(Vec::new()));
            }
            let records = crate::orchestrate_graph::run(
                ctx,
                crate::orchestrate_graph::GraphOrchestrateOptions {
                    nodes: &nodes,
                    cwd: std::path::Path::new(&cwd),
                    real_home,
                    timeout: std::time::Duration::from_secs(timeout_secs),
                },
            )?;
            Ok(ResponseData::OrchestrateGraphResult(records))
        }
        Request::AccountCapture { agent, name, label } => {
            // Captures only from this agent's divisi-managed home,
            // bootstrapped here if this is the first account operation for
            // this agent. The real ~/.claude etc. is never read for the
            // capture itself — only used to seed a brand-new isolated home's
            // non-credential config once (see agent_home::ensure_bootstrapped).
            let real_home = integrations::home_dir()?;
            let home = divisi_core::agent_home::ensure_bootstrapped(
                &ctx.dirs.homes_dir(),
                &real_home,
                &agent,
            )?;
            let info = divisi_core::account::capture(
                &ctx.dirs.accounts_dir(),
                &home,
                &agent,
                &name,
                label,
            )?;
            crate::state::open(&ctx.dirs.db_path())
                .and_then(|conn| {
                    crate::state::record_event(
                        &conn,
                        "account.captured",
                        &format!("{agent}/{name}"),
                    )
                })
                .ok();
            Ok(ResponseData::AccountProfile(info))
        }
        Request::AccountUse { agent, name } => {
            let real_home = integrations::home_dir()?;
            let home = divisi_core::agent_home::ensure_bootstrapped(
                &ctx.dirs.homes_dir(),
                &real_home,
                &agent,
            )?;
            let result =
                divisi_core::account::switch(&ctx.dirs.accounts_dir(), &home, &agent, &name)?;
            crate::state::open(&ctx.dirs.db_path())
                .and_then(|conn| {
                    crate::state::record_event(
                        &conn,
                        "account.switched",
                        &format!("{agent}/{name}"),
                    )
                })
                .ok();
            Ok(ResponseData::AccountSwitched(result))
        }
        Request::AccountList { agent } => Ok(ResponseData::AccountProfiles(
            divisi_core::account::list(&ctx.dirs.accounts_dir(), agent.as_deref())?,
        )),
        Request::AccountRemove { agent, name } => {
            if !divisi_core::account::remove(&ctx.dirs.accounts_dir(), &agent, &name)? {
                anyhow::bail!("no profile named '{name}' for agent '{agent}'");
            }
            Ok(ResponseData::Empty)
        }
        Request::AccountSetStatus {
            agent,
            name,
            status,
        } => {
            divisi_core::account::set_status(&ctx.dirs.accounts_dir(), &agent, &name, status)?;
            Ok(ResponseData::Empty)
        }
        Request::DockerEnable { agent, account } => {
            divisi_core::docker::set_enabled(
                &ctx.dirs.docker_registry_file(),
                &agent,
                account.as_deref(),
                true,
            )?;
            Ok(ResponseData::Empty)
        }
        Request::DockerDisable { agent, account } => {
            divisi_core::docker::set_enabled(
                &ctx.dirs.docker_registry_file(),
                &agent,
                account.as_deref(),
                false,
            )?;
            Ok(ResponseData::Empty)
        }
        Request::DockerStatus { agent } => {
            let settings =
                divisi_core::docker::status(&ctx.dirs.docker_registry_file(), agent.as_deref())?;
            let infos = settings
                .into_iter()
                .map(|s| {
                    let container_name =
                        divisi_core::docker::container_name(&s.agent, s.account.as_deref());
                    let running = crate::docker::is_running(&container_name).unwrap_or(None);
                    divisi_protocol::DockerContainerInfo {
                        agent: s.agent,
                        account: s.account,
                        container_name,
                        enabled: s.enabled,
                        running,
                    }
                })
                .collect();
            Ok(ResponseData::DockerContainerList(infos))
        }
        Request::DockerStop { agent, account } => {
            let container = divisi_core::docker::container_name(&agent, account.as_deref());
            crate::docker::stop(&container)?;
            let enabled = divisi_core::docker::is_enabled(
                &ctx.dirs.docker_registry_file(),
                &agent,
                account.as_deref(),
            )?;
            Ok(ResponseData::DockerContainerInfo(
                divisi_protocol::DockerContainerInfo {
                    agent,
                    account,
                    container_name: container,
                    enabled,
                    running: Some(false),
                },
            ))
        }
        Request::HooksEnable { agent } => {
            divisi_core::hooks::set_enabled(&ctx.dirs.hooks_registry_file(), &agent, true)?;
            let real_home = integrations::home_dir()?;
            let home = divisi_core::agent_home::ensure_bootstrapped(
                &ctx.dirs.homes_dir(),
                &real_home,
                &agent,
            )?;
            let settings_path = home.join(".claude/settings.json");
            let hook_command = format!(
                "{} internal claude-pretooluse-hook",
                resolve_divisi_binary_path()
            );
            let updated = divisi_agent_sdk::formats::claude_settings::apply_hook(
                &settings_path,
                &hook_command,
                divisi_core::hooks::CLAUDE_HOOK_TIMEOUT_SECS,
            )?;
            write_settings_with_backup(&settings_path, &updated)?;
            Ok(ResponseData::Empty)
        }
        Request::HooksDisable { agent } => {
            divisi_core::hooks::set_enabled(&ctx.dirs.hooks_registry_file(), &agent, false)?;
            let settings_path = ctx
                .dirs
                .homes_dir()
                .join(&agent)
                .join(".claude/settings.json");
            let hook_command = format!(
                "{} internal claude-pretooluse-hook",
                resolve_divisi_binary_path()
            );
            if let Some(updated) = divisi_agent_sdk::formats::claude_settings::remove_hook(
                &settings_path,
                &hook_command,
            )? {
                write_settings_with_backup(&settings_path, &updated)?;
            }
            Ok(ResponseData::Empty)
        }
        Request::HooksStatus => Ok(ResponseData::HooksStatus(divisi_core::hooks::status(
            &ctx.dirs.hooks_registry_file(),
        )?)),
        Request::ApprovalList => {
            let conn = preferences_db(ctx)?;
            let approvals = divisi_core::preferences::list_pending(&conn)?
                .into_iter()
                .map(to_approval_info)
                .collect();
            Ok(ResponseData::Approvals(approvals))
        }
        Request::ApprovalResolve {
            id,
            allow,
            remember,
        } => {
            let conn = preferences_db(ctx)?;
            divisi_core::preferences::resolve(&conn, id, allow, remember)?;
            Ok(ResponseData::Empty)
        }
        Request::PreferenceList => {
            let conn = preferences_db(ctx)?;
            let prefs = divisi_core::preferences::list_preferences(&conn)?
                .into_iter()
                .map(to_preference_info)
                .collect();
            Ok(ResponseData::Preferences(prefs))
        }
        Request::ProviderAdd {
            name,
            env_var_name,
            base_url,
            models,
        } => {
            let secret_name = format!("provider:{name}");
            divisi_core::providers::add(
                &ctx.dirs.providers_registry_file(),
                divisi_protocol::ProviderSpec {
                    name,
                    env_var_name,
                    secret_name,
                    base_url,
                    models,
                },
            )?;
            Ok(ResponseData::Empty)
        }
        Request::ProviderAddPreset { name } => {
            let preset = divisi_core::providers::preset(&name).ok_or_else(|| {
                anyhow::anyhow!("no such preset: {name} (see `divisi provider presets`)")
            })?;
            divisi_core::providers::add(&ctx.dirs.providers_registry_file(), preset.to_spec())?;
            Ok(ResponseData::Empty)
        }
        Request::ProviderPresetList => {
            let presets = divisi_core::providers::presets()
                .into_iter()
                .map(|p| divisi_protocol::ProviderPresetInfo {
                    name: p.name.to_string(),
                    env_var_name: p.env_var_name.to_string(),
                    base_url: p.base_url.to_string(),
                })
                .collect();
            Ok(ResponseData::ProviderPresets(presets))
        }
        Request::ProviderRemove { name } => {
            if !divisi_core::providers::remove(&ctx.dirs.providers_registry_file(), &name)? {
                anyhow::bail!("no such provider: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::ProviderList => Ok(ResponseData::Providers(divisi_core::providers::load(
            &ctx.dirs.providers_registry_file(),
        )?)),
        Request::ConfiguredProviderList => {
            let configured = divisi_core::providers::configured(
                &ctx.dirs.providers_registry_file(),
                &ctx.dirs.provider_keys_registry_file(),
            )?;
            Ok(ResponseData::Providers(configured))
        }
        Request::ProviderInspect { name } => {
            let provider =
                divisi_core::providers::find(&ctx.dirs.providers_registry_file(), &name)?
                    .ok_or_else(|| anyhow::anyhow!("no such provider: {name}"))?;
            Ok(ResponseData::Provider(provider))
        }
        Request::ProviderSetKey { name, value } => {
            let provider =
                divisi_core::providers::find(&ctx.dirs.providers_registry_file(), &name)?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "no such provider: {name} (add it first with `divisi provider add`)"
                        )
                    })?;
            let store = divisi_core::secrets::SecretTool;
            divisi_core::secrets::SecretStore::set(&store, &provider.secret_name, &value)?;
            Ok(ResponseData::Empty)
        }
        Request::ProviderSync {
            name,
            agents,
            dry_run,
            real_home,
        } => {
            let provider =
                divisi_core::providers::find(&ctx.dirs.providers_registry_file(), &name)?
                    .ok_or_else(|| anyhow::anyhow!("no such provider: {name}"))?;
            let store = divisi_core::secrets::SecretTool;
            let value = divisi_core::secrets::SecretStore::get(&store, &provider.secret_name)?
                .ok_or_else(|| anyhow::anyhow!("no key stored for provider '{name}'; run `divisi provider set-key {name} <value>` first"))?;
            let home_root = integrations::home_dir()?;
            let target_agents: Vec<String> = if agents.is_empty() {
                ctx.registry.iter().map(|a| a.name.clone()).collect()
            } else {
                agents
            };
            let mut results = Vec::new();
            for agent in target_agents {
                let home = if real_home {
                    home_root.clone()
                } else {
                    divisi_core::agent_home::ensure_bootstrapped(
                        &ctx.dirs.homes_dir(),
                        &home_root,
                        &agent,
                    )?
                };
                let mut result = divisi_agent_sdk::provider_sync::sync(
                    &agent,
                    &home,
                    &provider,
                    &value,
                    dry_run,
                )?;
                result.provider = name.clone();
                results.push(result);
            }
            Ok(ResponseData::ProviderSyncResults(results))
        }
        Request::ProviderAddKey {
            provider,
            label,
            agent,
            value,
        } => {
            let store = divisi_core::secrets::SecretTool;
            let secret_name = divisi_core::provider_keys::secret_name(&provider, &label);
            divisi_core::secrets::SecretStore::set(&store, &secret_name, &value)?;
            divisi_core::provider_keys::add(
                &ctx.dirs.provider_keys_registry_file(),
                divisi_protocol::ProviderKeySpec {
                    provider,
                    label,
                    agent,
                    secret_name,
                },
            )?;
            Ok(ResponseData::Empty)
        }
        Request::ProviderListKeys { provider } => {
            let keys = divisi_core::provider_keys::list_for_provider(
                &ctx.dirs.provider_keys_registry_file(),
                &provider,
            )?;
            Ok(ResponseData::ProviderKeys(keys))
        }
        Request::ProviderRemoveKey { provider, label } => {
            let key = divisi_core::provider_keys::find(
                &ctx.dirs.provider_keys_registry_file(),
                &provider,
                &label,
            )?
            .ok_or_else(|| anyhow::anyhow!("no such key: {provider}:{label}"))?;
            let store = divisi_core::secrets::SecretTool;
            divisi_core::secrets::SecretStore::delete(&store, &key.secret_name)?;
            divisi_core::provider_keys::remove(
                &ctx.dirs.provider_keys_registry_file(),
                &provider,
                &label,
            )?;
            Ok(ResponseData::Empty)
        }
        Request::ProviderKeySync {
            provider,
            label,
            agent,
            dry_run,
        } => {
            let provider_spec =
                divisi_core::providers::find(&ctx.dirs.providers_registry_file(), &provider)?
                    .ok_or_else(|| anyhow::anyhow!("no such provider: {provider}"))?;
            let key = divisi_core::provider_keys::find(
                &ctx.dirs.provider_keys_registry_file(),
                &provider,
                &label,
            )?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no such key: {provider}:{label} (add it first with `divisi provider add-key`)"
                )
            })?;
            let store = divisi_core::secrets::SecretTool;
            let value = divisi_core::secrets::SecretStore::get(&store, &key.secret_name)?
                .ok_or_else(|| anyhow::anyhow!("key '{provider}:{label}' has no value stored"))?;
            let real_home = integrations::home_dir()?;
            let home = divisi_core::agent_home::ensure_bootstrapped(
                &ctx.dirs.homes_dir(),
                &real_home,
                &agent,
            )?;
            let mut result = divisi_agent_sdk::provider_sync::sync(
                &agent,
                &home,
                &provider_spec,
                &value,
                dry_run,
            )?;
            result.provider = provider;
            Ok(ResponseData::ProviderSyncResults(vec![result]))
        }
        Request::ProviderSetBillingKey { provider, value } => {
            let store = divisi_core::secrets::SecretTool;
            divisi_core::secrets::SecretStore::set(&store, &format!("billing:{provider}"), &value)?;
            Ok(ResponseData::Empty)
        }
        Request::BillingProviderList => {
            let store = divisi_core::secrets::SecretTool;
            let infos = divisi_core::billing::builtin_registry()
                .into_iter()
                .map(|p| {
                    let configured = divisi_core::secrets::SecretStore::get(
                        &store,
                        &format!("billing:{}", p.provider),
                    )
                    .ok()
                    .flatten()
                    .is_some();
                    divisi_protocol::BillingProviderInfo {
                        provider: p.provider.to_string(),
                        verified: p.verified,
                        admin_key_env_hint: p.admin_key_env_hint.to_string(),
                        admin_key_configured: configured,
                        notes: if p.notes.is_empty() {
                            None
                        } else {
                            Some(p.notes.to_string())
                        },
                    }
                })
                .collect();
            Ok(ResponseData::BillingProviders(infos))
        }
        Request::ProviderListFree => {
            let infos = divisi_core::free_pool::FREE_PROVIDERS
                .iter()
                .map(|p| divisi_protocol::FreeProviderInfo {
                    id: p.id.to_string(),
                    display: p.display.to_string(),
                    signup_url: p.signup_url.to_string(),
                    rpm: p.limits.rpm,
                    rpd: p.limits.rpd,
                    tpm: p.limits.tpm,
                    tpd: p.limits.tpd,
                    free_note: p.free_note.to_string(),
                    disabled_reason: divisi_core::free_pool::default_disabled_reason(p.id).map(str::to_string),
                })
                .collect();
            Ok(ResponseData::FreeProviders(infos))
        }
        Request::ProviderPoolKeyState { platform, key_id, action, category, note } => {
            let conn = crate::state::open(&ctx.dirs.db_path())?;
            match action.as_str() {
                "disable" => {
                    let category = category.unwrap_or_else(|| "disabled".to_string());
                    divisi_core::pool_keys::disable_with_category(&conn, &platform, &key_id, &category, note.as_deref())?;
                }
                "enable" => divisi_core::pool_keys::enable_clearing_category(&conn, &platform, &key_id)?,
                "remove" => {
                    if !divisi_core::pool_keys::remove(&conn, &platform, &key_id)? {
                        anyhow::bail!("no pool key {platform}:{key_id}");
                    }
                    let store = divisi_core::secrets::SecretTool;
                    let _ = divisi_core::secrets::SecretStore::delete(&store, &divisi_core::pool_keys::secret_name(&platform, &key_id));
                }
                other => anyhow::bail!("unknown action {other} (disable, enable, remove)"),
            }
            // A disabled or removed key is not "benched": drop its cooldown rows so pool status counts only
            // keys that will come back on their own.
            let _ = conn.execute("DELETE FROM pool_cooldowns WHERE platform = ?1 AND key_id = ?2", rusqlite::params![platform, key_id]);
            Ok(ResponseData::Empty)
        }
        Request::ProviderAddFree { id, key, key_id } => {
            let provider = divisi_core::free_pool::by_id(&id).ok_or_else(|| {
                anyhow::anyhow!("no such free provider: {id} (see `divisi provider list-free`)")
            })?;
            divisi_core::free_pool::validate_key_shape(provider, &key).map_err(|e| anyhow::anyhow!(e))?;
            let conn = crate::state::open(&ctx.dirs.db_path())?;
            divisi_core::pool_keys::ensure_schema(&conn)?;
            // Live-verification finding: this used to hardcode key_id to
            // "default" unconditionally, so a second `add-free` call for
            // an already-keyed platform silently OVERWROTE the first key
            // (pool_provider_keys is keyed by (platform, key_id), and
            // pool_keys::add is an upsert on that pair) instead of adding
            // a second key to the pool's real capacity. An explicit
            // key_id still overwrites that one key on purpose (rotation);
            // omitting it now auto-picks a fresh, never-before-used id
            // for this platform instead of reusing "default".
            let key_id = match key_id {
                Some(explicit) => explicit,
                None => divisi_core::pool_keys::next_free_key_id(&divisi_core::pool_keys::list(&conn, Some(&id))?),
            };
            let store = divisi_core::secrets::SecretTool;
            let secret_name = divisi_core::pool_keys::secret_name(&id, &key_id);
            divisi_core::secrets::SecretStore::set(&store, &secret_name, &key)?;
            divisi_core::pool_keys::add(&conn, &id, &key_id)?;
            // Registering a fresh key value is an explicit "try this
            // again" signal, whether this is the first time or a
            // rotation over a key `mark_validated` previously disabled
            // (e.g. after a confirmed-bad kilo/xkiro key) -- the probe
            // right below re-evaluates it on real evidence regardless.
            divisi_core::pool_keys::enable(&conn, &id, &key_id)?;

            // Best-effort key validation — a failed/absent probe just
            // leaves the key unvalidated, it never fails the command
            // (spec §5.3: validation is advisory, not a gate).
            if let Some(ok) = probe_free_provider_key(provider, &key) {
                divisi_core::pool_keys::mark_validated(&conn, &id, &key_id, ok)?;
            }
            Ok(ResponseData::Empty)
        }
        Request::ProviderSyncPool => {
            let conn = crate::state::open(&ctx.dirs.db_path())?;
            divisi_core::pool_keys::ensure_schema(&conn)?;
            let existing = divisi_core::free_pool::load_pool_file(&ctx.dirs.free_pool_registry_file())?;
            let reconciled = divisi_core::free_pool::reconcile_pool_state(&existing, |id| {
                divisi_core::pool_keys::list(&conn, Some(id))
                    .map(|keys| keys.iter().any(|k| k.valid && !k.disabled))
                    .unwrap_or(false)
            });
            divisi_core::free_pool::save_pool_file(&ctx.dirs.free_pool_registry_file(), &reconciled)?;

            let providers_path = ctx.dirs.providers_registry_file();
            let mut synced = 0usize;
            for provider in divisi_core::free_pool::FREE_PROVIDERS {
                let name = format!("divisi-{}", provider.id);
                let env_var_name = format!("DIVISI_POOL_{}_API_KEY", provider.id.to_uppercase().replace('-', "_"));
                divisi_core::providers::add(
                    &providers_path,
                    divisi_protocol::ProviderSpec {
                        name: name.clone(),
                        env_var_name,
                        secret_name: format!("provider:{name}"),
                        base_url: if provider.base_url.is_empty() { None } else { Some(provider.base_url.to_string()) },
                        models: Vec::new(),
                    },
                )?;
                synced += 1;
            }
            Ok(ResponseData::PoolSyncResult { synced })
        }
        Request::ProviderKeyStatus { platform } => Ok(ResponseData::PoolKeyStatuses(provider_key_status_info(ctx, platform.as_deref())?)),
        Request::ProviderValidateKeys { platform } => {
            let conn = crate::state::open(&ctx.dirs.db_path())?;
            crate::pool::ensure_pool_schema(&conn)?;
            let providers: Vec<_> = divisi_core::free_pool::FREE_PROVIDERS
                .iter()
                .filter(|p| platform.as_deref().is_none_or(|want| want == p.id))
                .collect();
            let store = divisi_core::secrets::SecretTool;
            for provider in &providers {
                // Every keyed provider is checked with one tiny real chat call (on its best discovered
                // models when it lists them); its own validation URL is only the fallback.
                {
                    // A keyless provider (anonymous access) has no key to check.
                    if matches!(provider.auth, divisi_core::free_pool::Auth::Keyless(_)) {
                        continue;
                    }
                    for key in divisi_core::pool_keys::list(&conn, Some(provider.id))? {
                        let secret_name = divisi_core::pool_keys::secret_name(provider.id, &key.key_id);
                        let Some(secret) = divisi_core::secrets::SecretStore::get(&store, &secret_name)? else {
                            // Registered but nothing in the keyring: re-add it with `divisi provider add-free`.
                            conn.execute(
                                "INSERT INTO pool_key_notes (platform, key_id, note, at) VALUES (?1, ?2, ?3, ?4)
                                 ON CONFLICT(platform, key_id) DO UPDATE SET note = excluded.note, at = excluded.at",
                                rusqlite::params![provider.id, key.key_id, format!("no secret stored in the keyring for {secret_name}; re-add it with `divisi provider add-free {} --key-id {}`", provider.id, key.key_id), chrono::Utc::now().to_rfc3339()],
                            )?;
                            continue;
                        };
                        let (mut verdict, note) = chat_probe(&conn, provider, &secret);
                        // A provider's own validation URL is the fallback when the chat call is inconclusive.
                        if verdict.is_none() && provider.quirks.validate_url.is_some() {
                            verdict = probe_free_provider_key(provider, &secret);
                        }
                        if let Some(ok) = verdict {
                            divisi_core::pool_keys::mark_validated(&conn, provider.id, &key.key_id, ok)?;
                        }
                        conn.execute(
                            "INSERT INTO pool_key_notes (platform, key_id, note, at) VALUES (?1, ?2, ?3, ?4)
                             ON CONFLICT(platform, key_id) DO UPDATE SET note = excluded.note, at = excluded.at",
                            rusqlite::params![provider.id, key.key_id, note, chrono::Utc::now().to_rfc3339()],
                        )?;
                    }
                }
            }
            Ok(ResponseData::PoolKeyStatuses(pool_key_statuses(&conn, &ctx.dirs, platform.as_deref())?))
        }
        Request::PoolStatus => Ok(ResponseData::PoolStatus(pool_status_info(ctx)?)),
        Request::UsageShow { provider } => usage_summary(ctx, provider),
        Request::UsageRefresh => usage_summary(ctx, None),
        Request::AccountingQuery { query } => accounting_query(ctx, &query),
        Request::PluginAdd { plugin } => {
            divisi_core::plugins::add(&ctx.dirs.plugins_registry_file(), plugin)?;
            Ok(ResponseData::Empty)
        }
        Request::PluginRemove { name } => {
            if !divisi_core::plugins::remove(&ctx.dirs.plugins_registry_file(), &name)? {
                anyhow::bail!("no such plugin: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::PluginList => Ok(ResponseData::Plugins(divisi_core::plugins::load(
            &ctx.dirs.plugins_registry_file(),
        )?)),
        Request::PluginInspect { name } => {
            let plugin = divisi_core::plugins::find(&ctx.dirs.plugins_registry_file(), &name)?
                .ok_or_else(|| anyhow::anyhow!("no such plugin: {name}"))?;
            Ok(ResponseData::Plugin(plugin))
        }
        Request::PluginSync {
            name,
            agents,
            dry_run,
            real_home,
        } => {
            let plugin = divisi_core::plugins::find(&ctx.dirs.plugins_registry_file(), &name)?
                .ok_or_else(|| anyhow::anyhow!("no such plugin: {name}"))?;
            let home_root = integrations::home_dir()?;
            let target_agents: Vec<String> = if agents.is_empty() {
                ctx.registry.iter().map(|a| a.name.clone()).collect()
            } else {
                agents
            };
            let mut results = Vec::new();
            for agent in target_agents {
                let selector = if agent == "opencode" {
                    plugin
                        .opencode_module
                        .clone()
                        .unwrap_or_else(|| plugin.target.clone())
                } else {
                    plugin.target.clone()
                };
                let (applied, detail) =
                    match for_agent_with_custom(&agent, &ctx.dirs.agents_dir(), &ctx.registry) {
                        Some(adapter) if dry_run => (
                            false,
                            format!(
                                "dry run: would run `{} plugin install {selector}`",
                                adapter.command()
                            ),
                        ),
                        Some(adapter) => {
                            let home = if real_home {
                                home_root.clone()
                            } else {
                                divisi_core::agent_home::ensure_bootstrapped(
                                    &ctx.dirs.homes_dir(),
                                    &home_root,
                                    &agent,
                                )?
                            };
                            // `install_plugin` shells out to the agent's own CLI
                            // (e.g. `claude plugin install`), which writes that
                            // agent's config file itself — unlike the direct
                            // in-process writes elsewhere in this handler, that
                            // write never goes through `write_with_backup`'s
                            // `backup_before_write` call. When `real_home` is
                            // set this is about to touch a real, in-daily-use
                            // config, so snapshot the file we know it's going
                            // to modify before invoking the external command.
                            // Only `claude`'s config path is confirmed here
                            // (`~/.claude/settings.json`, per `claude plugin
                            // --help` on the reference machine); other agents'
                            // plugin-install config paths aren't confirmed
                            // (see `adapters.rs`'s per-adapter doc comments),
                            // so this snapshot is scoped to `claude` only.
                            if real_home && agent == "claude" {
                                divisi_agent_sdk::backup::backup_before_write(
                                    &home.join(".claude").join("settings.json"),
                                )?;
                            }
                            match adapter.install_plugin(
                                &selector,
                                &home,
                                std::time::Duration::from_secs(60),
                            ) {
                                Ok(outcome) if outcome.success => (true, "installed".to_string()),
                                Ok(outcome) => (
                                    false,
                                    format!(
                                        "exited with {:?}: {}",
                                        outcome.exit_code, outcome.stderr
                                    ),
                                ),
                                Err(err) => (false, err.to_string()),
                            }
                        }
                        None => (false, format!("no adapter for agent '{agent}'")),
                    };
                results.push(divisi_protocol::PluginInstallResult {
                    plugin: name.clone(),
                    agent,
                    applied,
                    detail,
                });
            }
            Ok(ResponseData::PluginSyncResults(results))
        }
        Request::FallbackSet { chain } => {
            divisi_core::fallback::set(&ctx.dirs.fallback_registry_file(), chain)?;
            Ok(ResponseData::Empty)
        }
        Request::FallbackList => Ok(ResponseData::FallbackChains(divisi_core::fallback::load(&ctx.dirs.fallback_registry_file())?)),
        Request::FallbackRemove { first } => {
            let removed = divisi_core::fallback::remove(&ctx.dirs.fallback_registry_file(), &first)?;
            anyhow::ensure!(removed, "no fallback chain starts with that entry");
            Ok(ResponseData::Empty)
        }
        Request::TaskHookAdd { on, command, agent, workspace } => {
            divisi_core::task_hooks::add(&ctx.dirs.task_hooks_registry_file(), divisi_protocol::TaskHookRule { on, command, agent, workspace })?;
            Ok(ResponseData::Empty)
        }
        Request::TaskHookList => Ok(ResponseData::TaskHooks(divisi_core::task_hooks::list(&ctx.dirs.task_hooks_registry_file())?)),
        Request::TaskHookRemove { command } => {
            let removed = divisi_core::task_hooks::remove(&ctx.dirs.task_hooks_registry_file(), &command)?;
            Ok(ResponseData::TaskHookRemoved(removed))
        }
        Request::TaskHookTest { command } => {
            divisi_core::task_hooks::test(&ctx.dirs.task_hooks_registry_file(), &command)?;
            Ok(ResponseData::Empty)
        }
        Request::PluginPresetList => {
            let presets = divisi_core::plugins::presets()
                .into_iter()
                .map(|p| divisi_protocol::PluginPresetInfo {
                    name: p.name.to_string(),
                    target: p.target.to_string(),
                })
                .collect();
            Ok(ResponseData::PluginPresets(presets))
        }
        Request::PluginAddPreset { name } => {
            let preset = divisi_core::plugins::preset(&name).ok_or_else(|| {
                anyhow::anyhow!("no such preset: {name} (see `divisi plugin presets`)")
            })?;
            divisi_core::plugins::add(&ctx.dirs.plugins_registry_file(), preset.to_spec())?;
            Ok(ResponseData::Empty)
        }
        Request::KgCreateEntity { name, entity_type } => {
            let conn = kg_db(ctx)?;
            crate::knowledge_graph::create_entity(&conn, &name, &entity_type)?;
            Ok(ResponseData::Empty)
        }
        Request::KgAddObservation { entity, content } => {
            let conn = kg_db(ctx)?;
            let id = crate::knowledge_graph::add_observation(&conn, &entity, &content)?;
            Ok(ResponseData::KgEntityId(id))
        }
        Request::KgCreateRelation {
            from,
            to,
            relation_type,
        } => {
            let conn = kg_db(ctx)?;
            let id = crate::knowledge_graph::create_relation(&conn, &from, &to, &relation_type)?;
            Ok(ResponseData::KgEntityId(id))
        }
        Request::KgDeleteEntity { name } => {
            let conn = kg_db(ctx)?;
            if !crate::knowledge_graph::delete_entity(&conn, &name)? {
                anyhow::bail!("no such entity: {name}");
            }
            Ok(ResponseData::Empty)
        }
        Request::KgGetEntity { name } => {
            let conn = kg_db(ctx)?;
            let entity = crate::knowledge_graph::get_entity(&conn, &name)?
                .ok_or_else(|| anyhow::anyhow!("no such entity: {name}"))?;
            Ok(ResponseData::KgEntity(entity))
        }
        Request::KgQuery { term } => {
            let conn = kg_db(ctx)?;
            Ok(ResponseData::KgEntities(crate::knowledge_graph::query(
                &conn, &term,
            )?))
        }
        Request::KgReadGraph => {
            let conn = kg_db(ctx)?;
            Ok(ResponseData::KgGraph(crate::knowledge_graph::read_graph(
                &conn,
            )?))
        }
        Request::CacheSet {
            key,
            value,
            ttl_secs,
        } => {
            let url = redis_url()?;
            crate::redis_backend::set(&url, &key, &value, ttl_secs)?;
            Ok(ResponseData::Empty)
        }
        Request::CacheGet { key } => {
            let url = redis_url()?;
            Ok(ResponseData::CacheValue(crate::redis_backend::get(
                &url, &key,
            )?))
        }
        Request::CacheDelete { key } => {
            let url = redis_url()?;
            if !crate::redis_backend::delete(&url, &key)? {
                anyhow::bail!("no such key: {key}");
            }
            Ok(ResponseData::Empty)
        }
        Request::CacheList { pattern } => {
            let url = redis_url()?;
            Ok(ResponseData::CacheKeys(crate::redis_backend::list_keys(
                &url, &pattern,
            )?))
        }
        Request::CacheStatus => {
            let url = crate::redis_backend::resolve_url();
            let reachable = url
                .as_deref()
                .map(|u| crate::redis_backend::ping(u).is_ok())
                .unwrap_or(false);
            Ok(ResponseData::CacheStatus {
                configured: url.is_some(),
                url,
                reachable,
            })
        }
        Request::VectorUpsert {
            collection,
            id,
            vector,
            payload,
        } => {
            let url = qdrant_url()?;
            crate::qdrant_backend::upsert_point(&url, &collection, id, &vector, payload)?;
            Ok(ResponseData::Empty)
        }
        Request::VectorSearch {
            collection,
            vector,
            limit,
        } => {
            let url = qdrant_url()?;
            Ok(ResponseData::VectorHits(crate::qdrant_backend::search(
                &url,
                &collection,
                &vector,
                limit,
            )?))
        }
        Request::VectorDelete { collection, id } => {
            let url = qdrant_url()?;
            crate::qdrant_backend::delete_point(&url, &collection, id)?;
            Ok(ResponseData::Empty)
        }
        Request::VectorStatus => {
            let url = crate::qdrant_backend::resolve_url();
            let reachable = url
                .as_deref()
                .map(|u| crate::qdrant_backend::ping(u).is_ok())
                .unwrap_or(false);
            Ok(ResponseData::VectorStatus {
                configured: url.is_some(),
                url,
                reachable,
            })
        }
        Request::AgentInstall { name, dry_run } => Ok(ResponseData::AgentInstallResult(
            bootstrap::run_one(ctx, &name, dry_run)?,
        )),
        Request::Setup { dry_run } => Ok(ResponseData::SetupPlan(bootstrap::run(ctx, dry_run))),
        Request::InstallIntegrations { dry_run, real_home } => Ok(ResponseData::IntegrationResult(
            integrations::install_all(ctx, dry_run, real_home)?,
        )),
        Request::UninstallIntegrations { real_home } => Ok(ResponseData::IntegrationResult(
            integrations::uninstall_all(ctx, false, real_home)?,
        )),
        Request::ProfileList => Ok(ResponseData::Profiles(divisi_core::profile::list_profiles(
            &ctx.dirs,
        )?)),
        Request::ProfileUse { name } => {
            divisi_core::profile::use_profile(&ctx.dirs, &name)?;
            Ok(ResponseData::Empty)
        }

        // ---- coordinator (spec E27.02 §6) ---------------------------
        Request::SessionNew { cwd } => {
            let conn = coordinator_db(ctx)?;
            let s = crate::coordinator::session::new_session(&conn, std::path::Path::new(&cwd))?;
            Ok(ResponseData::Session(session_info(s)))
        }
        Request::SessionList => {
            let conn = coordinator_db(ctx)?;
            let out = crate::coordinator::session::list(&conn)?.into_iter().map(session_info).collect();
            Ok(ResponseData::Sessions(out))
        }
        Request::SessionClose { session_id } => {
            let conn = coordinator_db(ctx)?;
            crate::coordinator::session::close(&conn, &session_id)?;
            Ok(ResponseData::Empty)
        }
        Request::GoalSubmit { session_id, text, mode, max_dispatches, max_minutes, agent } => {
            let conn = coordinator_db(ctx)?;
            divisi_core::redact::ensure_schema(&conn)?;
            let redact_store = divisi_core::redact::RedactStore { conn: &conn };
            let secret_store = divisi_core::secrets::SecretTool;
            let (text, _aliases) = divisi_core::redact::scan_and_replace(&redact_store, &secret_store, &session_id, &text)?;
            // E29: don't start a duplicate goal for an ask that's already
            // in flight (possibly from a different session/prompt) — hand
            // back the existing goal instead.
            if let Some(existing) = crate::coordinator::goal::find_overlapping(&conn, &text)? {
                return Ok(ResponseData::GoalId(existing.id));
            }
            let gmode = mode
                .as_deref()
                .and_then(|m| crate::coordinator::graph::GoalMode::parse(m).ok())
                .unwrap_or(crate::coordinator::graph::GoalMode::Auto);
            let cfg = crate::coordinator::routing::CoordinatorConfig::load(&ctx.dirs);
            // `careful` (single loop) defaults to a small iteration cap
            // (the prototype's 6) rather than the 25-dispatch goal budget.
            let default_dispatches = if gmode == crate::coordinator::graph::GoalMode::Careful {
                6
            } else {
                cfg.max_dispatches_per_goal
            };
            let g = crate::coordinator::goal::create(
                &conn,
                &session_id,
                &text,
                gmode,
                max_dispatches.unwrap_or(default_dispatches),
                max_minutes.unwrap_or(cfg.max_goal_minutes),
            )?;
            // Planning is a model call that can take minutes under load, so it runs on its own thread
            // and the daemon tick drives the goal once it has a graph (the scheduler skips a goal that
            // is still `planning`). A planning failure leaves the goal blocked and recoverable.
            let (plan_ctx, goal_id, agent) = (ctx.clone(), g.id.clone(), agent.clone());
            std::thread::spawn(move || match coordinator_db(&plan_ctx) {
                Ok(mut conn) => {
                    if let Err(e) = crate::coordinator::plan_goal(&plan_ctx, &mut conn, &goal_id, agent.as_deref()) {
                        let _ = crate::coordinator::goal::set_blocked(&conn, &goal_id, &format!("planning failed: {e:#}"));
                    }
                }
                Err(e) => tracing::warn!(goal = %goal_id, error = %e, "planning a submitted goal failed"),
            });
            Ok(ResponseData::GoalId(g.id))
        }
        Request::GoalStatus { goal_id } => {
            let conn = coordinator_db(ctx)?;
            let g = crate::coordinator::goal::get(&conn, &goal_id)?
                .ok_or_else(|| anyhow::anyhow!("no such goal: {goal_id}"))?;
            let task_tokens = |task_id: Option<i64>| -> (Option<i64>, Option<i64>, bool) {
                let Some(tid) = task_id else { return (None, None, false) };
                match crate::task::get(&conn, tid) {
                    Ok(Some(t)) => (t.prompt_tokens, t.completion_tokens, t.tokens_estimated),
                    _ => (None, None, false),
                }
            };
            let mut total_p = 0i64;
            let mut total_c = 0i64;
            let mut any_est = false;
            let nodes: Vec<_> = crate::coordinator::goal::load_graph(&conn, &goal_id)?
                .nodes
                .into_iter()
                .map(|n| {
                    let (pt, ct, est) = task_tokens(n.task_id);
                    total_p += pt.unwrap_or(0);
                    total_c += ct.unwrap_or(0);
                    any_est |= est;
                    divisi_protocol::NodeView {
                        id: n.id,
                        desc: n.desc,
                        kind: n.kind.as_str().to_string(),
                        effort: n.effort.as_str().to_string(),
                        agent: n.agent,
                        depends_on: n.depends_on,
                        status: n.status.as_str().to_string(),
                        task_id: n.task_id,
                        attempts: n.attempts,
                        prompt_tokens: pt,
                        completion_tokens: ct,
                        tokens_estimated: est,
                    }
                })
                .collect();
            let recent_events = crate::coordinator::events::for_goal(&conn, &goal_id, 40)?
                .into_iter()
                .map(coordinator_event)
                .collect();
            Ok(ResponseData::GoalView(divisi_protocol::GoalView {
                goal: goal_summary(&g),
                blocked_reason: g.blocked_reason.clone(),
                result_summary: g.result_summary.clone(),
                nodes,
                recent_events,
                total_prompt_tokens: total_p,
                total_completion_tokens: total_c,
                any_tokens_estimated: any_est,
            }))
        }
        Request::GoalList { session_id } => {
            let conn = coordinator_db(ctx)?;
            let out = crate::coordinator::goal::list(&conn, session_id.as_deref())?
                .iter()
                .map(goal_summary)
                .collect();
            Ok(ResponseData::Goals(out))
        }
        Request::GoalAmend { goal_id, text } => {
            let conn = coordinator_db(ctx)?;
            let g = crate::coordinator::goal::get(&conn, &goal_id)?
                .ok_or_else(|| anyhow::anyhow!("no such goal: {goal_id}"))?;
            // `budget=N` raises the dispatch cap and re-opens a blocked goal;
            // `minutes=N` raises the wall-clock cap the same way (a goal
            // blocked on elapsed time, not spent dispatches, re-blocks
            // immediately if only the dispatch cap moves); `capacity-budget=N`
            // (E28 spec §8) raises this goal's `max_capacity_waits_per_goal`
            // override; anything else is recorded as an amendment note.
            if let Some(n) = text.strip_prefix("budget=").and_then(|s| s.trim().parse::<u32>().ok()) {
                crate::coordinator::goal::raise_dispatch_cap(&conn, &goal_id, n)?;
            } else if let Some(n) = text.strip_prefix("minutes=").and_then(|s| s.trim().parse::<u32>().ok()) {
                crate::coordinator::goal::raise_time_cap(&conn, &goal_id, n)?;
            } else if let Some(n) = text.strip_prefix("capacity-budget=").and_then(|s| s.trim().parse::<u32>().ok()) {
                crate::coordinator::goal::raise_capacity_budget(&conn, &goal_id, n)?;
            } else if let Some(n) = text.strip_prefix("capacity-minutes=").and_then(|s| s.trim().parse::<u32>().ok()) {
                crate::coordinator::goal::raise_capacity_wait_minutes(&conn, &goal_id, n)?;
            } else if let Some(v) = text.strip_prefix("auto-merge=").map(|s| s.trim()) {
                let enabled = v.eq_ignore_ascii_case("true");
                if !enabled && !v.eq_ignore_ascii_case("false") {
                    anyhow::bail!("auto-merge=... needs true or false, got {v:?}");
                }
                crate::coordinator::goal::set_auto_merge(&conn, &goal_id, enabled)?;
            } else {
                crate::coordinator::events::append(
                    &conn,
                    &g.session_id,
                    Some(&goal_id),
                    crate::coordinator::events::EventKind::Message,
                    &format!("amendment: {text}"),
                )?;
            }
            // E28 spec §9.2: every real amend counts as a human touching
            // this goal — self-heal's coordinator category checks this
            // before auto-editing it.
            crate::coordinator::goal::mark_human_edited(&conn, &goal_id)?;
            // An answer to a goal parked for input is what it was waiting for: pick it back up. Resuming
            // can re-plan (a model call) and the daemon's tick drives it afterwards, so neither runs
            // inside this request: an amend from the CLI or the notch used to hang for minutes.
            if g.status == crate::coordinator::graph::GoalStatus::WaitingInput {
                let ctx = ctx.clone();
                std::thread::spawn(move || match coordinator_db(&ctx) {
                    Ok(mut conn) => {
                        if let Err(e) = crate::coordinator::resume_goal(&ctx, &mut conn, &goal_id) {
                            tracing::warn!(goal = %goal_id, error = %e, "resuming an amended goal failed");
                        }
                    }
                    Err(e) => tracing::warn!(goal = %goal_id, error = %e, "resuming an amended goal failed"),
                });
            }
            Ok(ResponseData::Empty)
        }
        Request::GoalCancel { goal_id } => {
            let conn = coordinator_db(ctx)?;
            crate::coordinator::goal::set_status(
                &conn,
                &goal_id,
                crate::coordinator::graph::GoalStatus::Cancelled,
            )?;
            Ok(ResponseData::Empty)
        }
        Request::GoalResume { goal_id } => {
            let mut conn = coordinator_db(ctx)?;
            crate::coordinator::resume_goal(ctx, &mut conn, &goal_id)?;
            let _ = crate::coordinator::drive_goal(ctx, &mut conn, registry, &goal_id);
            Ok(ResponseData::Empty)
        }
        Request::GoalRetryNode { goal_id, node_id, agent } => {
            let mut conn = coordinator_db(ctx)?;
            crate::coordinator::retry_node(ctx, &mut conn, registry, &goal_id, &node_id, agent.as_deref())?;
            Ok(ResponseData::Empty)
        }
        Request::GoalMergeList => {
            let conn = coordinator_db(ctx)?;
            let out = divisi_core::pending_merge::list_pending(&conn)?
                .into_iter()
                .map(to_pending_merge_info)
                .collect();
            Ok(ResponseData::PendingMerges(out))
        }
        Request::GoalMergeShow { id } => {
            let conn = coordinator_db(ctx)?;
            let pm = divisi_core::pending_merge::get(&conn, id)?
                .ok_or_else(|| anyhow::anyhow!("no such pending merge: {id}"))?;
            let cwd = crate::coordinator::session::get(&conn, &pm.session_id)?
                .map(|s| s.cwd)
                .ok_or_else(|| anyhow::anyhow!("no such session: {}", pm.session_id))?;
            let repo_root = divisi_core::project_context::resolve(std::path::Path::new(&cwd))
                .repo_root
                .ok_or_else(|| anyhow::anyhow!("goal {} has no resolvable repo root", pm.goal_id))?;
            let diff = divisi_core::worktree::diff(std::path::Path::new(&repo_root), &pm.branch)?;
            Ok(ResponseData::PendingMergeDiff(to_pending_merge_info(pm), diff))
        }
        Request::GoalMergeResolve { id, allow } => {
            let conn = coordinator_db(ctx)?;
            let pm = divisi_core::pending_merge::get(&conn, id)?
                .ok_or_else(|| anyhow::anyhow!("no such pending merge: {id}"))?;
            let resolved = divisi_core::pending_merge::resolve(&conn, id, allow)?;
            if allow {
                let cwd = crate::coordinator::session::get(&conn, &pm.session_id)?
                    .map(|s| s.cwd)
                    .ok_or_else(|| anyhow::anyhow!("no such session: {}", pm.session_id))?;
                let repo_root = divisi_core::project_context::resolve(std::path::Path::new(&cwd))
                    .repo_root
                    .ok_or_else(|| anyhow::anyhow!("goal {} has no resolvable repo root", pm.goal_id))?;
                match divisi_core::worktree::merge(std::path::Path::new(&repo_root), &pm.branch) {
                    Ok(output) => {
                        crate::coordinator::events::append(
                            &conn,
                            &pm.session_id,
                            Some(&pm.goal_id),
                            crate::coordinator::events::EventKind::Merged,
                            &format!("{} ({}) merged after human confirmation:\n{output}", pm.dep_node_id, pm.branch),
                        )?;
                    }
                    Err(e) => {
                        crate::coordinator::events::append(
                            &conn,
                            &pm.session_id,
                            Some(&pm.goal_id),
                            crate::coordinator::events::EventKind::MergeFailed,
                            &format!("{} ({}) confirmed merge failed: {e:#}", pm.dep_node_id, pm.branch),
                        )?;
                        return Err(e);
                    }
                }
            } else {
                crate::coordinator::events::append(
                    &conn,
                    &pm.session_id,
                    Some(&pm.goal_id),
                    crate::coordinator::events::EventKind::Message,
                    &format!("{} ({}) merge rejected by human", pm.dep_node_id, pm.branch),
                )?;
            }
            Ok(ResponseData::PendingMerges(vec![to_pending_merge_info(resolved)]))
        }
        Request::SessionEvents { session_id, since_event_id } => {
            let conn = coordinator_db(ctx)?;
            let out = crate::coordinator::events::since(&conn, &session_id, since_event_id)?
                .into_iter()
                .map(coordinator_event)
                .collect();
            Ok(ResponseData::CoordinatorEvents(out))
        }
        Request::AgentAuth { probe, deep, agents } => {
            let conn = coordinator_db(ctx)?;
            Ok(ResponseData::AgentAuth(crate::agent_auth::report(ctx, &conn, probe, deep, &agents)?))
        }
        Request::AgentCooldownClear { agent } => {
            let conn = coordinator_db(ctx)?;
            crate::agent_cooldown::clear(&conn, &agent)?;
            Ok(ResponseData::Empty)
        }
        Request::ChatSend { session, text, surface, mode, agent } => {
            let conn = coordinator_db(ctx)?;
            let mut cfg = crate::assistant::gate::ChatConfig::load(&ctx.dirs);
            if let Some(m) = mode.filter(|m| ["auto", "plan", "careful", "dry"].contains(&m.as_str())) {
                cfg.default_mode = m;
            }
            let model = crate::assistant::chat::PoolModel { ctx, conn: &conn };
            Ok(ResponseData::Chat(crate::assistant::chat::chat_send(ctx, &conn, &model, &cfg, session.as_deref(), &text, &surface, agent.as_deref())?))
        }
        Request::ChatHistory { session, since_event_id } => {
            let conn = coordinator_db(ctx)?;
            let session_id = match session {
                Some(id) => id,
                None => crate::coordinator::session::main_thread(&conn)?.id,
            };
            let events = crate::coordinator::events::since(&conn, &session_id, since_event_id)?.into_iter().map(coordinator_event).collect();
            Ok(ResponseData::Chat(divisi_protocol::ChatOutcome { session_id, events }))
        }
        Request::ChatConfirm { approval_id, allow, remember } => {
            let conn = coordinator_db(ctx)?;
            let cfg = crate::assistant::gate::ChatConfig::load(&ctx.dirs);
            let model = crate::assistant::chat::PoolModel { ctx, conn: &conn };
            Ok(ResponseData::Chat(crate::assistant::chat::chat_confirm(ctx, &conn, &model, &cfg, approval_id, allow, remember)?))
        }
        Request::CoordinatorStatus => Ok(ResponseData::CoordinatorSnapshot(coordinator_status_info(ctx)?)),
        Request::NotchSnapshot => Ok(ResponseData::NotchSnapshot(divisi_protocol::NotchSnapshotInfo {
            pool: pool_status_info(ctx)?,
            keys: provider_key_status_info(ctx, None)?,
            coordinator: coordinator_status_info(ctx)?,
            agents: notch_agents(ctx),
            recent_tasks: recent_task_briefs(ctx),
            agent_usage: agent_usage_stats(ctx),
        })),
    }
}

/// Agents as the notch shows them. Live finding (2026-09-24): the notch used the static login check,
/// so agents a real call had verified (claude, codex) still read "needs login"/"unverified", and
/// agents switched off on purpose (`disabled_agents`) looked broken. Disabled agents are left out; a
/// stored real-call probe (`divisi agent auth --probe`) overrides the static check.
fn notch_agents(ctx: &Context) -> Vec<divisi_protocol::AgentInfo> {
    use divisi_protocol::AuthState;
    let cfg = crate::coordinator::routing::CoordinatorConfig::load(&ctx.dirs);
    let conn = crate::state::open(&ctx.dirs.db_path()).ok();
    let probed: std::collections::HashMap<String, String> = conn
        .as_ref()
        .and_then(|conn| {
            let mut st = conn.prepare("SELECT agent, category FROM agent_auth_probe").ok()?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?;
            Some(rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    // A real task completed in the last day is stronger evidence than any probe.
    let since = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let worked: std::collections::HashSet<String> = conn
        .as_ref()
        .and_then(|conn| {
            let mut st = conn.prepare("SELECT DISTINCT agent FROM tasks WHERE status = 'completed' AND updated_at > ?1").ok()?;
            let rows = st.query_map([&since], |r| r.get::<_, String>(0)).ok()?;
            Some(rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    agent_list_info_cached(ctx)
        .into_iter()
        .filter(|a| !cfg.disabled_agents.iter().any(|d| d == &a.name))
        // Not installed / cannot run headless: not part of the pool, so not a login problem to show.
        .filter(|a| !matches!(probed.get(&a.name).map(String::as_str), Some("not_installed" | "not_dispatchable")))
        .map(|mut a| {
            if worked.contains(&a.name) {
                a.authenticated = AuthState::Authenticated;
                return a;
            }
            match probed.get(&a.name).map(String::as_str) {
                Some("authed" | "exhausted" | "no_auth_needed") => a.authenticated = AuthState::Authenticated,
                Some("needs_login") => a.authenticated = AuthState::NotAuthenticated,
                _ => {}
            }
            a
        })
        .collect()
}

fn agent_usage_stats(ctx: &Context) -> Vec<divisi_protocol::AgentLocalStats> {
    let Ok(conn) = crate::state::open(&ctx.dirs.db_path()) else { return Vec::new() };
    crate::task::local_stats_by_agent(&conn).unwrap_or_default()
}

/// Best-effort: a task-table read failure leaves the notch's task list empty
/// rather than failing the whole snapshot.
fn recent_task_briefs(ctx: &Context) -> Vec<divisi_protocol::NotchTaskBrief> {
    let Ok(conn) = crate::state::open(&ctx.dirs.db_path()) else { return Vec::new() };
    let Ok(tasks) = crate::task::list_recent(&conn, 8) else { return Vec::new() };
    tasks
        .into_iter()
        .map(|t| {
            let first_line = t.description.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let description: String = first_line.chars().take(110).collect();
            divisi_protocol::NotchTaskBrief {
                id: t.id,
                agent: t.agent,
                status: format!("{:?}", t.status).to_lowercase(),
                description,
                updated_at: t.updated_at,
            }
        })
        .collect()
}

/// Agent discovery probes every registered agent (~1s for the full registry),
/// which is far too slow for the notch's couple-of-seconds poll and changes
/// rarely, so the composite reuses a recent result. `Request::AgentList`
/// itself stays uncached.
fn agent_list_info_cached(ctx: &Context) -> Vec<divisi_protocol::AgentInfo> {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<Mutex<Option<(Instant, Vec<divisi_protocol::AgentInfo>)>>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(30);
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cell.lock() {
        if let Some((at, agents)) = guard.as_ref() {
            if at.elapsed() < TTL {
                return agents.clone();
            }
        }
    }
    let fresh = agent_list_info(ctx);
    if let Ok(mut guard) = cell.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

/// Shared with `Request::AgentList` and the `NotchSnapshot` composite
/// (E30 Phase 7) -- same logic either way, never forked.
fn agent_list_info(ctx: &Context) -> Vec<divisi_protocol::AgentInfo> {
    // Parallelized across agents for the same reason `status()` is — see
    // `cached_discover`'s doc comment.
    std::thread::scope(|scope| {
        let handles: Vec<_> = ctx.registry.iter().map(|def| scope.spawn(|| to_agent_info(def, ctx))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

/// Shared with `Request::ProviderKeyStatus` and `NotchSnapshot`.
fn provider_key_status_info(ctx: &Context, platform: Option<&str>) -> anyhow::Result<Vec<divisi_protocol::PoolKeyStatusInfo>> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    crate::pool::ensure_pool_schema(&conn)?;
    pool_key_statuses(&conn, &ctx.dirs, platform)
}

/// Shared with `Request::PoolStatus` and `NotchSnapshot`.
fn pool_status_info(ctx: &Context) -> anyhow::Result<divisi_protocol::PoolStatusInfo> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    crate::pool::ensure_pool_schema(&conn)?;
    let now = crate::pool::ledger::now_ms();

    let mut benched = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT platform, model, key_id, until_ms, provenance FROM pool_cooldowns WHERE until_ms > ?1")?;
        let rows = stmt.query_map(rusqlite::params![now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?))
        })?;
        for row in rows {
            let (platform, model, key_id, until_ms, provenance) = row?;
            benched.push(divisi_protocol::PoolBenchedKey { platform, model, key_id, remaining_secs: ((until_ms - now).max(0) / 1000) as u64, provenance });
        }
    }

    // Stateless healthy-ratio snapshot -- entry/exit grace hysteresis
    // (spec §6.5) needs a persisted DegradeState this iteration doesn't
    // wire into the daemon yet (documented follow-up); this reports the
    // instantaneous ratio, not a debounced mode with a "degraded since
    // <ts>" timestamp.
    let enabled_providers: Vec<_> =
        divisi_core::free_pool::FREE_PROVIDERS.iter().filter(|p| divisi_core::free_pool::default_disabled_reason(p.id).is_none()).collect();
    let usable = enabled_providers
        .iter()
        .filter(|p| divisi_core::pool_keys::list(&conn, Some(p.id)).map(|ks| ks.iter().any(|k| !k.disabled)).unwrap_or(false))
        .count();
    let ratio = crate::pool::degrade::healthy_ratio(usable, enabled_providers.len());
    let degraded = ratio < 0.5 && enabled_providers.len() >= 3;

    Ok(divisi_protocol::PoolStatusInfo { degraded, healthy_ratio: ratio, benched })
}

/// Shared with `Request::CoordinatorStatus` and `NotchSnapshot`.
fn coordinator_status_info(ctx: &Context) -> anyhow::Result<divisi_protocol::CoordinatorSnapshot> {
    let conn = coordinator_db(ctx)?;
    let all = crate::coordinator::goal::list(&conn, None)?;
    let pick = |want: crate::coordinator::graph::GoalStatus| all.iter().filter(|g| g.status == want).map(goal_summary).collect::<Vec<_>>();
    let cfg = crate::coordinator::routing::CoordinatorConfig::load(&ctx.dirs);
    let mut health = crate::coordinator::routing::PoolHealth::probe(&ctx.registry, &conn);
    health.disable(&cfg.disabled_agents);
    let mut running_per_agent: std::collections::BTreeMap<String, usize> = Default::default();
    {
        let mut stmt = conn.prepare("SELECT agent, COUNT(*) FROM graph_nodes WHERE status = 'running' GROUP BY agent")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
        for row in rows {
            let (a, c) = row?;
            running_per_agent.insert(a, c);
        }
    }
    let pool = ctx
        .registry
        .iter()
        .map(|a| divisi_protocol::PoolAgentStatus {
            agent: a.name.clone(),
            running: running_per_agent.get(&a.name).copied().unwrap_or(0),
            cap: a.max_concurrency.map(|c| c as usize),
            rate_limited: health.rate_limited.contains(&a.name),
        })
        .collect();
    Ok(divisi_protocol::CoordinatorSnapshot {
        running_goals: pick(crate::coordinator::graph::GoalStatus::Running),
        queued_goals: pick(crate::coordinator::graph::GoalStatus::Queued),
        blocked_goals: pick(crate::coordinator::graph::GoalStatus::Blocked),
        waiting_goals: pick(crate::coordinator::graph::GoalStatus::WaitingOnCapacity),
        pool,
        max_parallel: cfg.max_parallel,
    })
}

fn memory_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    memory::ensure_schema(&conn)?;
    Ok(conn)
}

/// Builds the Usage page's full summary: real `$` from every configured
/// billing-supported provider key (best-effort — one provider's API
/// failure doesn't blank out the others, it just gets skipped, since a
/// billing endpoint being briefly unreachable shouldn't hide every other
/// provider's real numbers) plus local-only run stats for every agent
/// that has no billing data at all. `provider_filter` narrows to one
/// provider's keys when set (used by `divisi usage show --provider`).
fn usage_summary(ctx: &Context, provider_filter: Option<String>) -> anyhow::Result<ResponseData> {
    let store = divisi_core::secrets::SecretTool;
    let mut provider_usage = Vec::new();

    let providers = divisi_core::billing::builtin_registry();
    for billing_provider in providers.iter().filter(|p| p.supported) {
        if let Some(filter) = &provider_filter {
            if filter != billing_provider.provider {
                continue;
            }
        }
        // A missing/unreachable keychain (e.g. secret-tool not installed)
        // means "no billing key configured for this provider" here, same
        // as everywhere else this codebase treats a missing optional
        // external capability — not a reason to fail the whole Usage
        // page for every other provider too.
        let Some(admin_key) = divisi_core::secrets::SecretStore::get(
            &store,
            &format!("billing:{}", billing_provider.provider),
        )
        .ok()
        .flatten() else {
            continue;
        };
        let keys = divisi_core::provider_keys::list_for_provider(
            &ctx.dirs.provider_keys_registry_file(),
            billing_provider.provider,
        )?;
        // openrouter has no separate admin key — its "admin key" *is* a
        // regular inference key, and usage is scoped to whichever key
        // authenticates the call, so it needs one fetch per labeled key
        // rather than one org-wide fetch like anthropic/openai.
        if billing_provider.provider == "openrouter" && !keys.is_empty() {
            for key in &keys {
                let Some(value) = divisi_core::secrets::SecretStore::get(&store, &key.secret_name)
                    .ok()
                    .flatten()
                else {
                    continue;
                };
                if let Ok(mut records) =
                    crate::billing::fetch_usage("openrouter", &value, chrono::Utc::now())
                {
                    for record in &mut records {
                        record.key_label = Some(key.label.clone());
                        record.agent = key.agent.clone();
                    }
                    provider_usage.extend(records);
                }
            }
            continue;
        }
        let since = chrono::Utc::now() - chrono::Duration::days(30);
        if let Ok(mut records) =
            crate::billing::fetch_usage(billing_provider.provider, &admin_key, since)
        {
            for record in &mut records {
                if let Some(label) = &record.key_label {
                    record.agent = keys
                        .iter()
                        .find(|k| &k.label == label)
                        .and_then(|k| k.agent.clone());
                }
            }
            provider_usage.extend(records);
        }
    }

    // `Iterator::sum()` on an empty f64 sequence yields -0.0 (a real IEEE
    // 754 quirk, confirmed by direct execution, not assumed), which then
    // prints as the confusing "$-0.0000" — -0.0 == 0.0 is true, so this
    // normalizes only that case to plain positive zero for display,
    // without touching a real (non-zero) total.
    let total_usd: f64 = provider_usage.iter().map(|r| r.cost_usd).sum();
    let total_usd = if total_usd == 0.0 { 0.0 } else { total_usd };
    let conn = task_db(ctx)?;
    let agent_local_stats = crate::task::local_stats_by_agent(&conn)?;

    Ok(ResponseData::Usage(divisi_protocol::UsageSummary {
        provider_usage,
        agent_local_stats,
        total_usd,
        last_refreshed: Some(chrono::Utc::now().to_rfc3339()),
    }))
}

fn accounting_query(ctx: &Context, query: &ProtoAccountingQuery) -> anyhow::Result<ResponseData> {
    let conn = task_db(ctx)?;
    crate::accounting::ensure_schema(&conn)?;
    let q = crate::accounting::AccountingQuery {
        execution_id: query.execution_id.clone(),
        trace_id: query.trace_id.clone(),
        agent: query.agent.clone(),
        provider: query.provider.clone(),
        event_type: query.event_type.clone(),
    };
    let result = crate::accounting::query_usage_events(&conn, &q)?;
    let proto_result = divisi_protocol::AccountingResult {
        events: result.events,
        breakdowns: result.breakdowns,
        totals: result.totals,
    };
    Ok(ResponseData::Accounting(proto_result))
}

fn task_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    crate::task::ensure_schema(&conn)?;
    // Also needed here (not just via memory_db/kg_db/notes_db) since
    // task::run's context preamble reads all three best-effort on every
    // run — see build_context_preamble.
    crate::memory::ensure_schema(&conn)?;
    divisi_core::notes::ensure_schema(&conn)?;
    crate::knowledge_graph::ensure_schema(&conn)?;
    // agent == "divisi-pool" reads pool_provider_keys/pool_usage/etc on
    // every run (task::execute's special case) — needed here, not just
    // server.rs's startup reconcile, so the in-process (no-daemon)
    // fallback path also has the tables before the first pool run.
    crate::pool::ensure_pool_schema(&conn)?;
    Ok(conn)
}

fn notes_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    divisi_core::notes::ensure_schema(&conn)?;
    Ok(conn)
}

pub(crate) fn coordinator_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    crate::task::ensure_schema(&conn)?; // graph nodes point at tasks rows
    crate::coordinator::ensure_coordinator_schema(&conn)?;
    Ok(conn)
}

fn goal_summary(g: &crate::coordinator::goal::Goal) -> divisi_protocol::GoalSummary {
    divisi_protocol::GoalSummary {
        id: g.id.clone(),
        session_id: g.session_id.clone(),
        text: g.text.clone(),
        mode: g.mode.as_str().to_string(),
        status: g.status.as_str().to_string(),
        dispatches: g.dispatches,
        max_dispatches: g.max_dispatches,
        created_at: g.created_at.clone(),
        capacity_reason: g.capacity_reason.clone(),
        capacity_eta: g.earliest_retry_at_ms.and_then(chrono::DateTime::from_timestamp_millis).map(|d| d.to_rfc3339()),
        blocked_reason: g.blocked_reason.clone(),
    }
}

fn session_info(s: crate::coordinator::session::Session) -> divisi_protocol::SessionInfo {
    divisi_protocol::SessionInfo {
        id: s.id,
        cwd: s.cwd,
        title: s.title,
        created_at: s.created_at,
        updated_at: s.updated_at,
        status: s.status,
    }
}

pub(crate) fn coordinator_event(e: crate::coordinator::events::Event) -> divisi_protocol::CoordinatorEvent {
    divisi_protocol::CoordinatorEvent { id: e.id, goal_id: e.goal_id, ts: e.ts, kind: e.kind, body: e.body }
}

fn documents_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    crate::documents::ensure_schema(&conn)?;
    memory::ensure_schema(&conn)?;
    Ok(conn)
}

fn to_document_info(doc: crate::documents::DocumentInfo) -> divisi_protocol::DocumentInfo {
    divisi_protocol::DocumentInfo {
        id: doc.id,
        title: doc.title,
        project: doc.project,
        source_path: doc.source_path,
        extracted_chars: doc.extracted_chars,
        memory_id: doc.memory_id,
        ingested_at: doc.ingested_at,
    }
}

/// The `divisi` binary's absolute path, so the hook command written into
/// an isolated home's settings.json works regardless of what `PATH` looks
/// like when Claude Code spawns the hook process — falls back to the bare
/// command name (relying on `PATH`) if `which` can't find it, same
/// resolution style `divisi-agent-sdk::discover` already uses.
fn resolve_divisi_binary_path() -> String {
    std::process::Command::new("which")
        .arg("divisi")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "divisi".to_string())
}

fn write_settings_with_backup(
    path: &std::path::Path,
    contents: &serde_json::Value,
) -> anyhow::Result<()> {
    if path.exists() {
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let backup = path.with_extension(format!("json.bak-{timestamp}"));
        let _ = std::fs::copy(path, &backup);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(contents)?)?;
    Ok(())
}

/// Best-effort probe of one free-pool provider's key against its declared
/// `quirks.validate_url`. `None` means "nothing to probe" (no quirk, or
/// no base url) — distinct from `Some(false)`, an actual failed probe.
/// Shared by `ProviderAddFree` (probe once at registration) and
/// `ProviderValidateKeys` (re-probe on demand).
/// `Some(true)` when the key answers (or is merely rate limited), `Some(false)` when the provider
/// rejects it or wants payment, `None` when the result says nothing about the key.
fn chat_probe(conn: &rusqlite::Connection, provider: &divisi_core::free_pool::FreeProvider, key: &str) -> (Option<bool>, String) {
    use crate::pool::client::{ChatMessage, PoolError, PoolRequest};
    // A free tier can refuse one model (a "pro" one, say) and serve the next, so try a few.
    if !crate::pool::models::discoverable(provider) {
        // Native wires pick their own model: one call says whether the key works.
        let req = PoolRequest {
            messages: vec![ChatMessage { role: "user".into(), content: "Reply with the single word OK.".into() }],
            max_tokens: Some(16),
            ..Default::default()
        };
        return match crate::pool::client::native::dispatch_for_wire(&req, provider, key) {
            Ok(_) => (Some(true), "answered".into()),
            Err(PoolError::RateLimited { .. }) => (Some(true), "rate limited (key works)".into()),
            Err(PoolError::AuthFailed) => (Some(false), "key rejected (401)".into()),
            Err(e) => (None, format!("{e:?}").chars().take(200).collect()),
        };
    }
    let models = crate::pool::models::best(conn, provider, key, crate::pool::ledger::now_ms());
    if models.is_empty() {
        let why = match crate::pool::models::discover(provider, key) {
            Ok(_) => "the model list has no chat model".to_string(),
            Err(e) => format!("{e:#}").chars().take(200).collect(),
        };
        let rejected = why.contains("401") || why.contains("403");
        return (rejected.then_some(false), format!("could not list models: {why}"));
    }
    let mut seen = Vec::new();
    for model in models.into_iter().take(4) {
        let req = PoolRequest {
            messages: vec![ChatMessage { role: "user".into(), content: "Reply with the single word OK.".into() }],
            model: Some(model.clone()),
            max_tokens: Some(16),
            ..Default::default()
        };
        match crate::pool::client::native::dispatch_for_wire(&req, provider, key) {
            Ok(_) => return (Some(true), format!("{model} -> answered")),
            Err(PoolError::RateLimited { .. }) => return (Some(true), format!("{model} -> rate limited (key works)")),
            Err(PoolError::AuthFailed) => return (Some(false), format!("{model} -> key rejected (401)")),
            Err(e) => {
                let e = format!("{e:?}");
                seen.push(format!("{model} -> {}", e.chars().take(160).collect::<String>()));
            }
        }
    }
    (None, seen.join(" | "))
}

fn probe_free_provider_key(provider: &divisi_core::free_pool::FreeProvider, key: &str) -> Option<bool> {
    let path = provider.quirks.validate_url?;
    if provider.base_url.is_empty() {
        return None;
    }
    let url = format!("{}{}", provider.base_url, path);
    let client = reqwest::blocking::Client::new();
    Some(client.get(&url).bearer_auth(key).timeout(provider.timeout).send().map(|resp| resp.status().is_success()).unwrap_or(false))
}

/// Builds the same `PoolKeyStatusInfo` rows `ProviderKeyStatus` and
/// `ProviderValidateKeys` both return — factored out so validating first
/// and then reporting status doesn't duplicate this per-provider
/// cooldown/headroom computation.
fn pool_key_statuses(conn: &rusqlite::Connection, dirs: &divisi_core::DivisiDirs, platform: Option<&str>) -> anyhow::Result<Vec<divisi_protocol::PoolKeyStatusInfo>> {
    let pool_state = divisi_core::free_pool::load_pool_file(&dirs.free_pool_registry_file())?;
    let providers: Vec<_> = divisi_core::free_pool::FREE_PROVIDERS.iter().filter(|p| platform.is_none_or(|want| want == p.id)).collect();
    let now = crate::pool::ledger::now_ms();
    let mut statuses = Vec::new();
    for provider in providers {
        let keys = divisi_core::pool_keys::list(conn, Some(provider.id))?;
        let key = keys.first();
        let disabled_reason = divisi_core::free_pool::default_disabled_reason(provider.id)
            .map(str::to_string)
            .or_else(|| pool_state.get(provider.id).and_then(|e| e.disabled_reason.clone()));

        // Cooldown/headroom are per (platform, model, key_id); this
        // iteration has no live per-provider model list (D3 seam
        // documented in pool_agent.rs), so `key-status` reports the
        // one nominal model matching what `client.rs`/`pool_agent.rs`
        // actually dispatch against: `provider.id` itself.
        let key_id = key.map(|k| k.key_id.as_str()).unwrap_or("default");
        let cooldown = match crate::pool::cooldown::is_benched(conn, provider.id, provider.id, key_id, now) {
            Ok(Some(until_ms)) => {
                let remaining_s = (until_ms - now).max(0) / 1000;
                format!("benched {remaining_s}s")
            }
            Ok(None) => "clear".to_string(),
            Err(_) => "n/a".to_string(),
        };
        let since = crate::pool::ledger::next_utc_midnight_ms(now) - 24 * 60 * 60 * 1000;
        // Requests counted by our own ledger, over every registered key -- the
        // pool rotates across all of them, so the first key alone undercounts.
        let requests_today: u64 = if keys.is_empty() {
            0
        } else {
            keys.iter().map(|k| crate::pool::ledger::recorded_requests_since(conn, provider.id, &k.key_id, since).unwrap_or(0)).sum()
        };
        let first_key_used: u64 = crate::pool::ledger::recorded_requests_since(conn, provider.id, key_id, since).unwrap_or(0);
        let headroom = provider
            .limits
            .rpd
            .map(|limit| format!("{}/{} rpd", limit.saturating_sub(first_key_used as u32), limit))
            .unwrap_or_else(|| "unbounded/unknown".to_string());

        // A key that has served a real successful call is working whether or
        // not the provider offers a validation endpoint to probe.
        let proven: std::collections::HashSet<String> = conn
            .prepare("SELECT DISTINCT key_id FROM pool_outcomes WHERE platform = ?1 AND ok = 1")
            .and_then(|mut st| st.query_map([provider.id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>())
            .unwrap_or_default();
        let works = |k: &divisi_core::pool_keys::PoolProviderKey| k.valid || proven.contains(&k.key_id);
        let counts = divisi_core::auth_class::KeyCounts {
            total: keys.len() as u32,
            valid: keys.iter().filter(|k| !k.disabled && works(k)).count() as u32,
            invalid: keys.iter().filter(|k| !k.disabled && !works(k) && k.last_validated_at.is_some()).count() as u32,
            unvalidated: keys.iter().filter(|k| !k.disabled && !works(k) && k.last_validated_at.is_none()).count() as u32,
            disabled: keys.iter().filter(|k| k.disabled).count() as u32,
        };
        let keyless = matches!(provider.auth, divisi_core::free_pool::Auth::Keyless(_));
        let auth_state = divisi_core::auth_class::provider_auth_state(keyless, divisi_core::free_pool::default_disabled_reason(provider.id), &counts);

        statuses.push(divisi_protocol::PoolKeyStatusInfo {
            platform: provider.id.to_string(),
            keyed: key.is_some(),
            // A provider is usable when any of its keys works; one dead key among valid ones made
            // openrouter/huggingface/xkiro look unverified (2026-09-24).
            valid: counts.valid > 0,
            last_validated_at: key.and_then(|k| k.last_validated_at.clone()),
            disabled_reason,
            cooldown,
            headroom,
            key_count: counts.total,
            keys_valid: counts.valid,
            keys_invalid: counts.invalid,
            keys_unvalidated: counts.unvalidated,
            keys_disabled: counts.disabled,
            notes: keys
                .iter()
                .filter(|k| !k.valid || k.disabled)
                .filter_map(|k| {
                    if let Some(c) = divisi_core::pool_keys::category(&conn, provider.id, &k.key_id) {
                        return Some(format!("{}: disabled ({c})", k.key_id));
                    }
                    conn.query_row("SELECT note FROM pool_key_notes WHERE platform = ?1 AND key_id = ?2", rusqlite::params![provider.id, k.key_id], |r| r.get::<_, String>(0))
                        .ok()
                        .map(|n| format!("{}: {n}", k.key_id))
                })
                .collect(),
            auth_kind: if keyless { "keyless" } else { "key" }.to_string(),
            auth_state: auth_state.to_string(),
            can_validate: provider.quirks.validate_url.is_some(),
            signup_url: provider.signup_url.to_string(),
            requests_today,
            rpd_limit: provider.limits.rpd,
            rpm_limit: provider.limits.rpm,
            tpm_limit: provider.limits.tpm,
            tpd_limit: provider.limits.tpd,
        });
    }
    Ok(statuses)
}

fn preferences_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    divisi_core::preferences::ensure_schema(&conn)?;
    Ok(conn)
}

fn to_approval_info(a: divisi_core::preferences::Approval) -> divisi_protocol::ApprovalInfo {
    let status = match a.status {
        divisi_core::preferences::ApprovalStatus::Pending => "pending",
        divisi_core::preferences::ApprovalStatus::Allowed => "allowed",
        divisi_core::preferences::ApprovalStatus::Denied => "denied",
        divisi_core::preferences::ApprovalStatus::Used => "used",
    };
    divisi_protocol::ApprovalInfo {
        id: a.id,
        resource: a.resource,
        context: a.context,
        status: status.to_string(),
        created_at: a.created_at,
        resolved_at: a.resolved_at,
    }
}

fn to_pending_merge_info(pm: divisi_core::pending_merge::PendingMerge) -> divisi_protocol::PendingMergeInfo {
    let status = match pm.status {
        divisi_core::pending_merge::PendingMergeStatus::Pending => "pending",
        divisi_core::pending_merge::PendingMergeStatus::Confirmed => "confirmed",
        divisi_core::pending_merge::PendingMergeStatus::Rejected => "rejected",
    };
    divisi_protocol::PendingMergeInfo {
        id: pm.id,
        goal_id: pm.goal_id,
        review_node_id: pm.review_node_id,
        dep_node_id: pm.dep_node_id,
        branch: pm.branch,
        status: status.to_string(),
        created_at: pm.created_at,
        resolved_at: pm.resolved_at,
    }
}

fn to_preference_info(p: divisi_core::preferences::Preference) -> divisi_protocol::PreferenceInfo {
    let decision = match p.decision {
        divisi_core::permissions::Decision::Deny => "deny",
        divisi_core::permissions::Decision::Ask => "ask",
        divisi_core::permissions::Decision::Allow => "allow",
    };
    divisi_protocol::PreferenceInfo {
        id: p.id,
        pattern: p.pattern,
        decision: decision.to_string(),
        confidence: p.confidence,
        learned_from: p.learned_from,
        created_at: p.created_at,
    }
}

fn redis_url() -> anyhow::Result<String> {
    crate::redis_backend::resolve_url().ok_or_else(|| {
        anyhow::anyhow!(
            "no Redis configured; set DIVISI_REDIS_URL to enable the working-memory cache"
        )
    })
}

fn qdrant_url() -> anyhow::Result<String> {
    crate::qdrant_backend::resolve_url().ok_or_else(|| {
        anyhow::anyhow!("no Qdrant configured; set DIVISI_QDRANT_URL to enable the vector store")
    })
}

fn kg_db(ctx: &Context) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    crate::knowledge_graph::ensure_schema(&conn)?;
    Ok(conn)
}

fn status(ctx: &Context) -> RuntimeStatus {
    // Parallelized across agents (see `cached_discover`'s doc comment) so a
    // cold cache costs roughly one agent's `which`+`--version` latency, not
    // the sum of all of them; a warm cache costs nothing but HashMap
    // lookups either way.
    let detected = std::thread::scope(|scope| {
        let handles: Vec<_> = ctx
            .registry
            .iter()
            .map(|a| scope.spawn(|| cached_discover(a, ctx).is_some_and(|d| d.detected)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(false))
            .filter(|detected| *detected)
            .count()
    });
    RuntimeStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        active_profile: ctx.resolved.active_profile.clone(),
        agents_known: ctx.registry.len(),
        agents_detected: detected,
        socket_path: ctx.dirs.socket_path().display().to_string(),
        db_path: ctx.dirs.db_path().display().to_string(),
    }
}

/// Per-agent discovery result, cached for `DISCOVERY_TTL`: `Discovery`
/// shells out `which` + `<cmd> --version` (two subprocess spawns), and
/// `Context::load()` builds a fresh `Context` on every single request (see
/// `server.rs`'s `handle_connection`) — without this cache, every
/// `Status`/`AgentList` call re-spawns those two processes for every
/// registered agent (15+ agents today), and the TUI fires both requests on
/// every refresh. An install/uninstall of an agent CLI is picked up within
/// one TTL window rather than instantly, same trade-off `daemon restart`
/// already exists for ($PATH itself is only re-read at daemon spawn time).
/// One lock per agent name (rather than one lock over the whole map) so
/// discovering different agents never blocks on each other — but
/// discovering the *same* agent from two concurrent requests (e.g.
/// `Status` and `AgentList`, both parallelizing across every registered
/// agent, fired together by `divisi-tui`'s `App::refresh`) now serializes
/// on that one agent's lock instead of both redundantly shelling out
/// `which`/`--version` for it at once: the second caller blocks until the
/// first finishes, then sees the just-cached result and returns instantly
/// rather than repeating the work. Without this, a cold TUI launch was
/// paying for up to 2x the subprocess spawns every registered agent needs.
static DISCOVERY_LOCKS: OnceLock<Mutex<HashMap<String, std::sync::Arc<Mutex<Option<(Discovery, Instant)>>>>>> = OnceLock::new();
const DISCOVERY_TTL: Duration = Duration::from_secs(45);

fn cached_discover(def: &AgentDefinition, ctx: &Context) -> Option<Discovery> {
    let locks = DISCOVERY_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let slot = locks.lock().unwrap().entry(def.name.clone()).or_default().clone();

    let mut slot = slot.lock().unwrap();
    if let Some((discovery, at)) = slot.as_ref() {
        if at.elapsed() < DISCOVERY_TTL {
            return Some(discovery.clone());
        }
    }
    let discovery = for_agent_with_custom(&def.name, &ctx.dirs.agents_dir(), &ctx.registry)?.discover();
    *slot = Some((discovery.clone(), Instant::now()));
    Some(discovery)
}

fn to_agent_info(def: &AgentDefinition, ctx: &Context) -> AgentInfo {
    let discovery = cached_discover(def, ctx);
    let isolated_home = ctx.dirs.homes_dir().join(&def.name);
    let authenticated = divisi_core::account::is_authenticated(&isolated_home, &def.name);
    AgentInfo {
        name: def.name.clone(),
        adapter: def.adapter.clone(),
        command: def.command.clone(),
        detected: discovery.as_ref().map(|d| d.detected).unwrap_or(false),
        version: discovery.and_then(|d| d.version),
        install_method: def.install_method.clone(),
        bootstrap_install: def.bootstrap_install.clone(),
        unverified: def.unverified,
        home_requirement: def.home_requirement,
        max_concurrency: def.max_concurrency,
        capabilities: def.capabilities,
        config_paths: def.config_paths.clone(),
        notes: def.notes.clone(),
        authenticated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use divisi_protocol::{ProviderSpec, Response};

    #[test]
    fn a_second_concurrent_doctor_is_rejected() {
        let first = DoctorGuard::acquire().expect("first doctor acquires");
        let err = DoctorGuard::acquire().expect_err("second is refused while the first is live");
        assert!(err.to_string().contains("doctor already running"));
        drop(first);
        // Once the in-flight run ends the flag is clear again.
        DoctorGuard::acquire().expect("doctor acquires again after the first finishes");
    }

    fn test_ctx(dir: &std::path::Path) -> Context {
        let dirs = divisi_core::DivisiDirs::from_root(dir.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
    }

    /// E29: exercises the real `GoalSubmit` → `SecretPromoteAlias` path
    /// end to end through `handle()`, against the real OS keychain (this
    /// environment has a working `secret-tool`) — the only way to prove
    /// the whole chain (redact on submit, embed session in the alias
    /// token, decrypt + promote + delete on request) actually works
    /// together, not just each piece in isolation.
    #[test]
    fn goal_submit_redacts_then_promote_alias_recovers_the_real_secret() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());

        let Response::Ok { data: ResponseData::Session(session) } =
            handle(&ctx, Request::SessionNew { cwd: dir.path().display().to_string() })
        else {
            panic!("SessionNew failed");
        };

        let response = handle(
            &ctx,
            Request::GoalSubmit {
                session_id: session.id.clone(),
                text: "call the api with sk-abcdEFGH1234567890abcdEFGH1234567890abcd please".to_string(),
                mode: Some("dry".to_string()),
                max_dispatches: None,
                max_minutes: None,
                agent: None,
            },
        );
        let Response::Ok { data: ResponseData::GoalId(goal_id) } = response else { panic!("GoalSubmit failed: {response:?}") };

        let Response::Ok { data: ResponseData::GoalView(view) } = handle(&ctx, Request::GoalStatus { goal_id }) else {
            panic!("GoalStatus failed");
        };
        assert!(!view.goal.text.contains("sk-abcdEFGH"), "goal text leaked the real key: {}", view.goal.text);
        let alias_start = view.goal.text.find("{{REDACTED:").expect("goal text has no alias token");
        let alias_end = view.goal.text[alias_start..].find("}}").unwrap() + alias_start + 2;
        let alias = view.goal.text[alias_start..alias_end].to_string();

        let secret_name = format!("e29-test-promoted-{}", crate::pool::ledger::now_ms());
        let response = handle(&ctx, Request::SecretPromoteAlias { alias, name: secret_name.clone() });
        assert!(matches!(response, Response::Ok { data: ResponseData::Empty }), "{response:?}");

        let Response::Ok { data: ResponseData::SecretValue(value) } = handle(&ctx, Request::SecretGet { name: secret_name.clone() }) else {
            panic!("SecretGet failed");
        };
        assert_eq!(value.as_deref(), Some("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));

        // cleanup: don't leave a real keychain entry behind.
        let _ = handle(&ctx, Request::SecretDelete { name: secret_name });
    }

    #[test]
    fn pool_status_reports_a_real_bench_after_cooldown_bench() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        crate::pool::cooldown::bench(&conn, "groq", "groq", "default", crate::pool::cooldown::BenchKind::Transient, crate::pool::ledger::now_ms()).unwrap();

        let response = handle(&ctx, Request::PoolStatus);
        let Response::Ok { data: ResponseData::PoolStatus(status) } = response else { panic!("unexpected response") };
        assert!(status.benched.iter().any(|b| b.platform == "groq" && b.key_id == "default"), "{:?}", status.benched);
    }

    /// E30 Phase 7: the composite `NotchSnapshot` op must bundle exactly
    /// what the four separate ops would return -- verified here by
    /// seeding the same real bench `pool_status_reports_a_real_bench_
    /// after_cooldown_bench` uses and asserting the bundled `pool` field
    /// shows it, not just that the handler returns *something*.
    #[test]
    fn notch_snapshot_bundles_the_same_pool_status_a_separate_call_would_return() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        crate::pool::cooldown::bench(&conn, "groq", "groq", "default", crate::pool::cooldown::BenchKind::Transient, crate::pool::ledger::now_ms()).unwrap();

        let separate = {
            let Response::Ok { data: ResponseData::PoolStatus(status) } = handle(&ctx, Request::PoolStatus) else { panic!("unexpected response") };
            status
        };
        let Response::Ok { data: ResponseData::NotchSnapshot(snapshot) } = handle(&ctx, Request::NotchSnapshot) else { panic!("unexpected response") };

        assert_eq!(snapshot.pool.degraded, separate.degraded);
        assert_eq!(snapshot.pool.healthy_ratio, separate.healthy_ratio);
        assert_eq!(snapshot.pool.benched.len(), separate.benched.len());
        assert!(snapshot.pool.benched.iter().any(|b| b.platform == "groq" && b.key_id == "default"), "{:?}", snapshot.pool.benched);
        // The other three legs are present and non-panicking to extract
        // -- their own dedicated handler tests cover correctness in depth.
        assert!(!snapshot.agents.is_empty(), "agent_list_info must never return empty against the real registry");
    }

    #[test]
    fn provider_key_status_reports_benched_and_headroom_after_real_usage() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        divisi_core::pool_keys::add(&conn, "groq", "default").unwrap();
        crate::pool::cooldown::bench(&conn, "groq", "groq", "default", crate::pool::cooldown::BenchKind::Transient, crate::pool::ledger::now_ms()).unwrap();
        crate::pool::ledger::record(&conn, "groq", "groq", "default", crate::pool::ledger::UsageKind::Request, 3, crate::pool::ledger::now_ms()).unwrap();

        let response = handle(&ctx, Request::ProviderKeyStatus { platform: Some("groq".to_string()) });
        let Response::Ok { data: ResponseData::PoolKeyStatuses(statuses) } = response else { panic!("unexpected response") };
        let groq = statuses.iter().find(|s| s.platform == "groq").unwrap();
        assert!(groq.cooldown.starts_with("benched"), "{}", groq.cooldown);
        // groq's catalog rpd limit is 1000 -- 3 recorded requests should
        // leave 997/1000 in the headroom string.
        assert_eq!(groq.headroom, "997/1000 rpd");
    }

    #[test]
    fn provider_validate_keys_skips_a_provider_with_no_validate_url_quirk() {
        // aihorde has no base_url/validate_url -- there's nothing to
        // probe, so this must be a genuine no-op (never touch the
        // network, never call mark_validated) rather than erroring or
        // guessing an endpoint.
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        divisi_core::pool_keys::add(&conn, "aihorde", "default").unwrap();

        let response = handle(&ctx, Request::ProviderValidateKeys { platform: Some("aihorde".to_string()) });
        let Response::Ok { data: ResponseData::PoolKeyStatuses(statuses) } = response else { panic!("unexpected response") };
        let aihorde = statuses.iter().find(|s| s.platform == "aihorde").unwrap();
        assert!(!aihorde.valid, "a skipped provider must not be marked valid");
        assert!(aihorde.last_validated_at.is_none(), "a skipped provider must not get a validation timestamp");
    }

    #[test]
    fn provider_validate_keys_only_touches_the_requested_platform() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        divisi_core::pool_keys::add(&conn, "aihorde", "default").unwrap();
        divisi_core::pool_keys::add(&conn, "groq", "default").unwrap();

        let response = handle(&ctx, Request::ProviderValidateKeys { platform: Some("aihorde".to_string()) });
        let Response::Ok { data: ResponseData::PoolKeyStatuses(statuses) } = response else { panic!("unexpected response") };
        assert_eq!(statuses.len(), 1, "expected only the requested platform in the response, got {statuses:?}");
        assert_eq!(statuses[0].platform, "aihorde");
    }

    #[test]
    fn provider_sync_real_home_writes_the_actual_home() {
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let real_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", real_home.path());

        let store = divisi_core::secrets::SecretTool;
        divisi_core::secrets::SecretStore::set(&store, "test-provider-key", "sk-test").unwrap();
        divisi_core::providers::add(&ctx.dirs.providers_registry_file(), ProviderSpec {
            name: "testprov".into(),
            env_var_name: "ANTHROPIC_API_KEY".into(),
            secret_name: "test-provider-key".into(),
            base_url: None,
            models: Vec::new(),
        }).unwrap();

        let response = handle(&ctx, Request::ProviderSync { name: "testprov".into(), agents: vec!["claude".into()], dry_run: false, real_home: true });
        assert!(matches!(response, Response::Ok { .. }));
        assert!(real_home.path().join(".claude/settings.json").exists());
        assert!(!dir.path().join("homes").join("claude").join(".claude/settings.json").exists());

        std::env::remove_var("HOME");
    }

    #[test]
    fn plugin_sync_real_home_backs_up_claude_settings_before_install_plugin_runs() {
        // Fix 2: `install_plugin` shells out to `claude plugin install`,
        // which writes ~/.claude/settings.json itself — outside
        // `write_with_backup`'s `backup_before_write` call. This proves the
        // handler snapshots that file *before* invoking the external
        // command when `real_home` is set, regardless of whether the
        // subprocess call itself succeeds (it won't in this sandbox, since
        // no real `claude` binary is on PATH — that's fine, the backup must
        // still have happened).
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let real_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", real_home.path());

        let settings_path = real_home.path().join(".claude").join("settings.json");
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        std::fs::write(&settings_path, r#"{"pre_existing": true}"#).unwrap();

        divisi_core::plugins::add(
            &ctx.dirs.plugins_registry_file(),
            divisi_protocol::PluginSpec { name: "test-plugin".into(), target: "test-plugin@marketplace".into(), opencode_module: None },
        )
        .unwrap();

        let response = handle(&ctx, Request::PluginSync { name: "test-plugin".into(), agents: vec!["claude".into()], dry_run: false, real_home: true });
        assert!(matches!(response, Response::Ok { .. }));

        // The original file is untouched (install_plugin never actually ran
        // successfully against it in this sandbox) but a backup snapshot of
        // its pre-install content must now exist alongside it.
        assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), r#"{"pre_existing": true}"#);
        let backups: Vec<_> = std::fs::read_dir(settings_path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1, "expected exactly one backup snapshot of settings.json before install_plugin ran");
        assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), r#"{"pre_existing": true}"#);

        std::env::remove_var("HOME");
    }

    #[test]
    fn plugin_sync_real_home_does_not_back_up_when_no_pre_existing_settings() {
        // A fresh install (no prior settings.json) has nothing to back up —
        // `backup_before_write` returns `None` rather than erroring, and
        // `PluginSync` must not fail because of it.
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let real_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", real_home.path());

        divisi_core::plugins::add(
            &ctx.dirs.plugins_registry_file(),
            divisi_protocol::PluginSpec { name: "test-plugin".into(), target: "test-plugin@marketplace".into(), opencode_module: None },
        )
        .unwrap();

        let response = handle(&ctx, Request::PluginSync { name: "test-plugin".into(), agents: vec!["claude".into()], dry_run: false, real_home: true });
        assert!(matches!(response, Response::Ok { .. }));
        assert!(!real_home.path().join(".claude").join("settings.json").exists());

        std::env::remove_var("HOME");
    }

    #[test]
    fn worktree_merge_preview_then_apply_round_trips_a_real_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(repo.path()).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("README.md"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);

        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::task::ensure_schema(&conn).unwrap();
        let task_id = crate::task::create_for_cwd(&conn, "test task", "claude", repo.path()).unwrap();

        let worktree_path = tempfile::tempdir().unwrap().path().join(format!("task-{task_id}"));
        let branch = format!("divisi/task-{task_id}");
        divisi_core::worktree::add(repo.path(), &worktree_path, &branch).unwrap();
        std::fs::write(worktree_path.join("new-file.txt"), "from the worktree").unwrap();
        let run_in_worktree = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(&worktree_path).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run_in_worktree(&["add", "."]);
        run_in_worktree(&["commit", "-q", "-m", "add new-file"]);

        // A real run marks the task finished and records its worktree path
        // (see `execute()` in task.rs) — the merge handlers now require
        // both before they'll touch a task's branch (Finding 3).
        conn.execute(
            "UPDATE tasks SET status = 'completed', worktree_path = ?1 WHERE id = ?2",
            rusqlite::params![worktree_path.display().to_string(), task_id],
        )
        .unwrap();

        let preview = handle(&ctx, Request::WorktreeMergePreview { task_id });
        let Response::Ok { data: ResponseData::WorktreeDiff(diff) } = preview else {
            panic!("expected WorktreeDiff, got {preview:?}");
        };
        assert!(diff.contains("new-file.txt"));

        let apply = handle(&ctx, Request::WorktreeMergeApply { task_id });
        let Response::Ok { data: ResponseData::WorktreeMerged(result) } = apply else {
            panic!("expected WorktreeMerged, got {apply:?}");
        };
        assert_eq!(result.task_id, task_id);
        assert_eq!(result.branch, branch);
        assert!(repo.path().join("new-file.txt").is_file());
    }

    #[test]
    fn worktree_merge_preview_errors_for_an_unknown_task_id() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let response = handle(&ctx, Request::WorktreeMergePreview { task_id: 999999 });
        assert!(matches!(response, Response::Error { .. }));
    }

    #[test]
    fn worktree_merge_preview_errors_clearly_for_a_task_that_never_used_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());

        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(repo.path()).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("README.md"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);

        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        crate::task::ensure_schema(&conn).unwrap();
        // A task that ran (so it's not caught by the still-running check)
        // but WITHOUT ever calling `divisi_core::worktree::add` for it —
        // `task.worktree_path` stays `None`, exactly the case Finding 3's
        // fix must catch with a clear error, not a raw git error.
        let task_id = crate::task::create_for_cwd(&conn, "test task", "claude", repo.path()).unwrap();
        conn.execute("UPDATE tasks SET status = 'completed' WHERE id = ?1", rusqlite::params![task_id])
            .unwrap();

        let response = handle(&ctx, Request::WorktreeMergePreview { task_id });
        let Response::Error { message } = response else {
            panic!("expected Response::Error, got {response:?}");
        };
        assert!(
            message.contains("did not run in a worktree"),
            "expected a clear did-not-run-in-a-worktree error, got: {message}"
        );
    }
}
