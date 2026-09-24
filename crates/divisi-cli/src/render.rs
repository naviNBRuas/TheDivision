use divisi_protocol::{CheckStatus, InstallMethod, Response, ResponseData};

/// Prints a `Response`, either as pretty text or as JSON (`divisi ... --json`),
/// and exits non-zero on `Response::Error` so shell scripting behaves.
pub fn print(response: Response, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(&response).unwrap());
        if matches!(response, Response::Error { .. }) {
            std::process::exit(1);
        }
        return;
    }

    match response {
        Response::Error { message } => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        Response::Ok { data } => print_data(data),
    }
}

fn print_data(data: ResponseData) {
    match data {
        ResponseData::Status(s) => {
            println!("divisi runtime v{}", s.version);
            println!("  profile:  {}", s.active_profile);
            println!(
                "  agents:   {}/{} detected",
                s.agents_detected, s.agents_known
            );
            println!("  socket:   {}", s.socket_path);
            println!("  db:       {}", s.db_path);
        }
        ResponseData::Doctor(report) => {
            for check in report.checks {
                let icon = match check.status {
                    CheckStatus::Ok => "✓",
                    CheckStatus::Warn => "!",
                    CheckStatus::Fail => "✗",
                    CheckStatus::Skipped => "-",
                };
                println!("[{icon}] {}: {}", check.name, check.detail);
            }
        }
        ResponseData::Agents(agents) => {
            for agent in agents {
                let dot = if agent.detected { "●" } else { "○" };
                let version = agent.version.as_deref().unwrap_or("-");
                let flag = if agent.unverified {
                    " (unverified)"
                } else {
                    ""
                };
                println!(
                    "{dot} {:<12} {:<20} {}{flag}",
                    agent.name,
                    version,
                    install_summary(&agent.install_method)
                );
            }
        }
        ResponseData::Agent(agent) => {
            println!("{}", agent.name);
            println!("  adapter:      {}", agent.adapter);
            println!("  command:      {}", agent.command);
            println!("  detected:     {}", agent.detected);
            println!(
                "  version:      {}",
                agent.version.as_deref().unwrap_or("-")
            );
            println!("  install:      {}", install_summary(&agent.install_method));
            if let Some(install) = &agent.bootstrap_install {
                println!(
                    "  bootstrap:    {} (source: {})",
                    install.command, install.source
                );
            }
            println!(
                "  capabilities: mcp={} lsp={} tools={} sessions={} streaming={} structured_output={} non_interactive_run={}",
                agent.capabilities.mcp,
                agent.capabilities.lsp,
                agent.capabilities.tools,
                agent.capabilities.sessions,
                agent.capabilities.streaming,
                agent.capabilities.structured_output,
                agent.capabilities.non_interactive_run
            );
            println!(
                "  home_requirement: {:?}{}",
                agent.home_requirement,
                agent
                    .max_concurrency
                    .map(|n| format!(" (max_concurrency: {n})"))
                    .unwrap_or_default()
            );
            if !agent.config_paths.is_empty() {
                println!("  config:       {}", agent.config_paths.join(", "));
            }
            if let Some(notes) = &agent.notes {
                println!("  notes:        {notes}");
            }
        }
        ResponseData::McpServers(servers) => {
            if servers.is_empty() {
                println!("(no mcp servers registered)");
            }
            for server in servers {
                let flag = if server.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                println!("{:<12} {:<40} [{flag}]", server.name, server.command);
            }
        }
        ResponseData::McpServer(server) => {
            println!("{}", server.name);
            println!("  command: {} {}", server.command, server.args.join(" "));
            println!("  enabled: {}", server.enabled);
            if !server.env.is_empty() {
                println!(
                    "  env:     {}",
                    server.env.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
        }
        ResponseData::McpPresets(presets) => {
            for p in presets {
                println!("{:<16} {} {}", p.name, p.command, p.args.join(" "));
            }
        }
        ResponseData::McpGatewayMode(enabled) => {
            println!(
                "gateway mode: {}",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        ResponseData::LspServers(servers) => {
            if servers.is_empty() {
                println!("(no lsp servers registered)");
            }
            for server in servers {
                let flag = if server.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                println!(
                    "{:<12} {:<30} {} [{flag}]",
                    server.name,
                    server.command,
                    server.extensions.join(",")
                );
            }
        }
        ResponseData::LspPresets(presets) => {
            for p in presets {
                println!(
                    "{:<14} {:<28} {}",
                    p.name,
                    format!("{} {}", p.command, p.args.join(" ")),
                    p.extensions.join(",")
                );
            }
        }
        ResponseData::LspServer(server) => {
            println!("{}", server.name);
            println!("  command:    {} {}", server.command, server.args.join(" "));
            println!("  extensions: {}", server.extensions.join(", "));
            println!("  enabled:    {}", server.enabled);
        }
        ResponseData::Tools(tools) => {
            if tools.is_empty() {
                println!("(no tools registered)");
            }
            for tool in tools {
                let flag = if tool.enabled { "enabled" } else { "disabled" };
                println!(
                    "{:<12} {:?} {:<40} [{flag}]",
                    tool.name, tool.risk_level, tool.description
                );
            }
        }
        ResponseData::Tool(tool) => {
            println!("{}", tool.name);
            println!("  description: {}", tool.description);
            println!("  risk:        {:?}", tool.risk_level);
            println!("  enabled:     {}", tool.enabled);
        }
        ResponseData::SecretNames(names) => {
            if names.is_empty() {
                println!("(no secrets stored)");
            }
            for name in names {
                println!("{name}");
            }
        }
        ResponseData::SecretValue(value) => match value {
            Some(v) => println!("{v}"),
            None => {
                eprintln!("(no such secret)");
                std::process::exit(1);
            }
        },
        ResponseData::Skills(skills) => {
            if skills.is_empty() {
                println!("(no skills installed)");
            }
            for skill in skills {
                println!("{skill}");
            }
        }
        ResponseData::SkillContents(entries) => {
            for entry in entries {
                println!("{entry}");
            }
        }
        ResponseData::SkillSynced { path } => println!("synced to {path}"),
        ResponseData::SkillStarters(starters) => {
            for s in starters {
                println!("{:<24} {}", s.name, s.description);
            }
        }
        ResponseData::MemoryId(id) => println!("stored as #{id}"),
        ResponseData::MemoryEntry(entry) => print_memory_entry(&entry),
        ResponseData::MemoryEntries(entries) => {
            if entries.is_empty() {
                println!("(no matching memories)");
            }
            for entry in entries {
                println!(
                    "#{:<5} {:<10} {:<18} {}",
                    entry.id,
                    format!("{:?}", entry.scope).to_lowercase(),
                    entry.created_at,
                    entry.title
                );
            }
        }
        ResponseData::NoteId(id) => println!("left as note #{id}"),
        ResponseData::Notes(notes) => {
            if notes.is_empty() {
                println!("(no notes)");
            }
            for note in notes {
                let read = if note.read_at.is_some() {
                    " (read)"
                } else {
                    ""
                };
                let to = note.to_agent.as_deref().unwrap_or("*");
                println!(
                    "#{:<5} {} -> {:<10} [{}]{read}  {}",
                    note.id, note.from_agent, to, note.topic, note.created_at
                );
                println!("        {}", note.content);
            }
        }
        ResponseData::Document(doc) => print_document(&doc),
        ResponseData::Documents(docs) => {
            if docs.is_empty() {
                println!("(no documents)");
            }
            for doc in docs {
                println!(
                    "#{:<5} {:<10} {:<30} {} chars  {}",
                    doc.id,
                    doc.project.as_deref().unwrap_or("-"),
                    doc.title,
                    doc.extracted_chars,
                    doc.ingested_at
                );
            }
        }
        ResponseData::Context(ctx) => {
            println!("cwd:       {}", ctx.cwd);
            println!(
                "repo root: {}",
                ctx.repo_root.as_deref().unwrap_or("(not a git repo)")
            );
            println!("branch:    {}", ctx.branch.as_deref().unwrap_or("-"));
            println!(
                "changed:   {}",
                if ctx.changed_files.is_empty() {
                    "(clean)".to_string()
                } else {
                    ctx.changed_files.join(", ")
                }
            );
            println!(
                "docs:      {}",
                if ctx.project_docs.is_empty() {
                    "(none found)".to_string()
                } else {
                    ctx.project_docs.join(", ")
                }
            );
        }
        ResponseData::Task(task) => print_task(&task),
        ResponseData::Tasks(tasks) => {
            if tasks.is_empty() {
                println!("(no tasks)");
            }
            for task in tasks {
                println!(
                    "#{:<5} {:<10} {:<10} {:<18} {}",
                    task.id,
                    format!("{:?}", task.status).to_lowercase(),
                    task.agent,
                    task.created_at,
                    one_line_description(&task.description, 80)
                );
            }
        }
        ResponseData::Workspaces(workspaces) => {
            if workspaces.is_empty() {
                println!("(no workspaces — run a task first)");
            }
            for w in workspaces {
                println!("{:<16} {:<4} tasks  last active {:<25} {}", w.name, w.task_count, w.last_activity_at, w.path);
            }
        }
        ResponseData::FallbackChains(chains) => {
            if chains.is_empty() {
                println!("(no fallback chains configured — `divisi fallback set <agent[:account]>...`)");
            }
            for chain in chains {
                let entries: Vec<String> = chain
                    .iter()
                    .map(|e| match &e.account {
                        Some(account) => format!("{}:{account}", e.agent),
                        None => e.agent.clone(),
                    })
                    .collect();
                println!("{}", entries.join(" -> "));
            }
        }
        ResponseData::TaskHooks(hooks) => {
            if hooks.is_empty() {
                println!("(no task hooks configured — `divisi task-hook add --on completed --command '...'`)");
            }
            for h in hooks {
                let scope = match (&h.agent, &h.workspace) {
                    (Some(a), Some(w)) => format!(" [agent={a} workspace={w}]"),
                    (Some(a), None) => format!(" [agent={a}]"),
                    (None, Some(w)) => format!(" [workspace={w}]"),
                    (None, None) => String::new(),
                };
                println!("on={:<28} {}{}", h.on.join(","), h.command, scope);
            }
        }
        ResponseData::TaskHookRemoved(count) => {
            if count == 0 {
                println!("no task hook had that exact command");
            } else {
                println!("removed {count} task hook(s)");
            }
        }
        ResponseData::OrchestrateResult(records) => {
            println!("Relay ({} step(s)):", records.len());
            for (i, r) in records.iter().enumerate() {
                let mark = match r.status {
                    divisi_protocol::TaskStatus::Completed => "✓",
                    divisi_protocol::TaskStatus::Failed => "✗",
                    _ => "·",
                };
                println!(
                    "  {mark} step {} — {} (task #{}): {}",
                    i + 1,
                    r.agent,
                    r.id,
                    r.summary.clone().unwrap_or_default()
                );
            }
            if let Some(last) = records.last() {
                if last.status == divisi_protocol::TaskStatus::Failed {
                    eprintln!(
                        "relay stopped early after a failed step; see `divisi task inspect {}`",
                        last.id
                    );
                    std::process::exit(1);
                }
            }
        }
        ResponseData::OrchestrateGraphResult(records) => {
            println!("Graph ({} node(s)):", records.len());
            for r in records {
                print_task(&r);
            }
        }
        ResponseData::AccountProfile(info) => print_account_profile(&info),
        ResponseData::AccountProfiles(profiles) => {
            if profiles.is_empty() {
                println!("(no captured account profiles)");
            }
            for p in profiles {
                let flag = if p.unverified_complete {
                    " (best-effort — not confirmed complete)"
                } else {
                    ""
                };
                let label = p.label.as_deref().unwrap_or("-");
                println!(
                    "{:<10} {:<16} {:<28} [{}] {}{flag}",
                    p.agent,
                    p.name,
                    label,
                    p.status.as_str(),
                    p.captured_at
                );
            }
        }
        ResponseData::AccountSwitched(result) => {
            println!("switched {} to profile '{}'", result.agent, result.name);
            for backup in result.backed_up {
                println!("  backup: {backup}");
            }
        }
        ResponseData::DockerContainerInfo(info) => print_docker_info(&info),
        ResponseData::DockerContainerList(infos) => {
            if infos.is_empty() {
                println!(
                    "(no agents/accounts configured for docker — see `divisi agent docker enable`)"
                );
            }
            for info in infos {
                print_docker_info(&info);
            }
        }
        ResponseData::HooksStatus(statuses) => {
            if statuses.is_empty() {
                println!("(no agents configured for mid-run permission interception — see `divisi agent hooks enable`)");
            }
            for (agent, enabled) in statuses {
                println!(
                    "{:<12} {}",
                    agent,
                    if enabled { "enabled" } else { "disabled" }
                );
            }
        }
        ResponseData::Approvals(approvals) => {
            if approvals.is_empty() {
                println!("(no pending approvals)");
            }
            for a in approvals {
                println!("#{:<5} [{}] {}", a.id, a.status, a.resource);
                if let Some(ctx) = &a.context {
                    println!("        {ctx}");
                }
            }
        }
        ResponseData::PendingMerges(merges) => {
            if merges.is_empty() {
                println!("(no pending merges)");
            }
            for m in merges {
                println!("#{:<5} [{}] {} <- {} (after {})", m.id, m.status, m.branch, m.dep_node_id, m.review_node_id);
                println!("        goal: {}", m.goal_id);
            }
        }
        ResponseData::PendingMergeDiff(m, diff) => {
            println!("#{:<5} [{}] {} <- {} (after {})", m.id, m.status, m.branch, m.dep_node_id, m.review_node_id);
            println!("        goal: {}", m.goal_id);
            println!();
            if diff.is_empty() {
                println!("(no diff — branch is even with HEAD)");
            } else {
                println!("{diff}");
            }
        }
        ResponseData::Preferences(prefs) => {
            if prefs.is_empty() {
                println!("(no learned preferences yet)");
            }
            for p in prefs {
                println!(
                    "#{:<5} {:<30} {:<7} conf={:.2}  {}",
                    p.id,
                    p.pattern,
                    p.decision,
                    p.confidence,
                    p.learned_from.as_deref().unwrap_or("-")
                );
            }
        }
        ResponseData::Provider(p) => print_provider(&p),
        ResponseData::Providers(providers) => {
            if providers.is_empty() {
                println!("(no providers registered)");
            }
            for p in providers {
                println!(
                    "{:<16} {:<20} {}",
                    p.name,
                    p.env_var_name,
                    p.base_url.as_deref().unwrap_or("-")
                );
            }
        }
        ResponseData::ProviderPresets(presets) => {
            for p in presets {
                println!("{:<14} {:<20} {}", p.name, p.env_var_name, p.base_url);
            }
        }
        ResponseData::ProviderSyncResults(results) => {
            for r in results {
                let mark = if r.applied { "✓" } else { "·" };
                println!("  {mark} {:<12} {} — {}", r.agent, r.config_path, r.detail);
                if let Some(backup) = r.backup_path {
                    println!("      backup: {backup}");
                }
            }
        }
        ResponseData::WorktreeDiff(diff) => {
            if diff.is_empty() {
                println!("(no differences — branch has nothing new to merge)");
            } else {
                println!("{diff}");
            }
        }
        ResponseData::WorktreeMerged(result) => {
            println!("Merged {} into the current branch.", result.branch);
            if !result.output.trim().is_empty() {
                println!("{}", result.output.trim());
            }
        }
        ResponseData::ProviderKeys(keys) => {
            if keys.is_empty() {
                println!("(no labeled keys for this provider)");
            }
            for k in keys {
                println!(
                    "{:<14} agent: {}",
                    k.label,
                    k.agent.as_deref().unwrap_or("-")
                );
            }
        }
        ResponseData::FreeProviders(providers) => {
            for p in providers {
                let limits = format!(
                    "rpm={} rpd={} tpm={} tpd={}",
                    p.rpm.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                    p.rpd.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                    p.tpm.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                    p.tpd.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                );
                println!("{:<14} {:<24} {}", p.id, p.display, limits);
                println!("               signup: {}", p.signup_url);
                println!("               note:   {}", p.free_note);
                if let Some(reason) = p.disabled_reason {
                    println!("               disabled by default: {reason}");
                }
            }
        }
        ResponseData::PoolSyncResult { synced } => {
            println!("synced {synced} free-pool providers into providers.toml and free-pool.toml");
        }
        ResponseData::PoolKeyStatuses(statuses) => {
            for s in statuses {
                let keyed = if s.keyed { if s.valid { "keyed, valid" } else { "keyed, unvalidated" } } else { "no key" };
                println!("{:<14} {:<20} cooldown={} headroom={}", s.platform, keyed, s.cooldown, s.headroom);
                if let Some(at) = s.last_validated_at {
                    println!("               last validated: {at}");
                }
                if let Some(reason) = s.disabled_reason {
                    println!("               disabled by default: {reason}");
                }
                for note in s.notes {
                    println!("               {note}");
                }
            }
        }
        ResponseData::BillingProviders(providers) => {
            for p in providers {
                let status = if !p.verified { "unverified" } else { "" };
                let admin = if p.admin_key_configured {
                    "admin key set"
                } else {
                    "admin key not set"
                };
                println!("{:<12} {:<11} {}", p.provider, status, admin);
                if let Some(notes) = p.notes {
                    println!("             {notes}");
                }
            }
        }
        ResponseData::Usage(summary) => {
            println!(
                "Provider spend (as of {}):",
                summary
                    .last_refreshed
                    .as_deref()
                    .unwrap_or("never refreshed")
            );
            if summary.provider_usage.is_empty() {
                println!("  (no provider usage data — configure a billing admin key with `divisi provider set-billing-key`)");
            }
            for r in &summary.provider_usage {
                println!(
                    "  {:<12} {:<10} {:<10} ${:.4}  [{} .. {}]",
                    r.provider,
                    r.key_label.as_deref().unwrap_or("-"),
                    r.agent.as_deref().unwrap_or("-"),
                    r.cost_usd,
                    r.period_start,
                    r.period_end
                );
            }
            println!("  TOTAL: ${:.4}", summary.total_usd);
            println!();
            println!("Connected agents (local stats only, no billing API):");
            for a in &summary.agent_local_stats {
                println!(
                    "  {:<14} runs: {:<5} avg: {}ms  last: {}",
                    a.agent,
                    a.run_count,
                    a.avg_duration_ms,
                    a.last_run_at.as_deref().unwrap_or("never")
                );
            }
        }
        ResponseData::Plugin(p) => {
            println!("name:            {}", p.name);
            println!("target:          {}", p.target);
            println!(
                "opencode_module: {}",
                p.opencode_module.as_deref().unwrap_or("-")
            );
        }
        ResponseData::Plugins(plugins) => {
            if plugins.is_empty() {
                println!("(no plugins registered)");
            }
            for p in plugins {
                println!(
                    "{:<16} {:<28} {}",
                    p.name,
                    p.target,
                    p.opencode_module.as_deref().unwrap_or("-")
                );
            }
        }
        ResponseData::PluginPresets(presets) => {
            for p in presets {
                println!("{:<28} {}", p.name, p.target);
            }
        }
        ResponseData::PluginSyncResults(results) => {
            for r in results {
                let mark = if r.applied { "✓" } else { "·" };
                println!("  {mark} {:<12} {}", r.agent, r.detail);
            }
        }
        ResponseData::KgEntityId(id) => println!("#{id}"),
        ResponseData::KgEntity(e) => print_kg_entity(&e),
        ResponseData::KgEntities(entities) => {
            if entities.is_empty() {
                println!("(no matching entities)");
            }
            for e in entities {
                println!(
                    "{:<20} {:<14} {} observation(s)",
                    e.name,
                    e.entity_type,
                    e.observations.len()
                );
            }
        }
        ResponseData::KgGraph(graph) => {
            println!("Entities:");
            for e in &graph.entities {
                println!("  {:<20} {}", e.name, e.entity_type);
                for o in &e.observations {
                    println!("    - {o}");
                }
            }
            println!("Relations:");
            for r in &graph.relations {
                println!(
                    "  {} --[{}]--> {}",
                    r.from_entity, r.relation_type, r.to_entity
                );
            }
        }
        ResponseData::CacheValue(value) => match value {
            Some(v) => println!("{v}"),
            None => {
                eprintln!("(no such key)");
                std::process::exit(1);
            }
        },
        ResponseData::CacheKeys(keys) => {
            if keys.is_empty() {
                println!("(no matching keys)");
            }
            for k in keys {
                println!("{k}");
            }
        }
        ResponseData::CacheStatus {
            configured,
            url,
            reachable,
        } => {
            if !configured {
                println!("Redis: not configured (set DIVISI_REDIS_URL to enable)");
            } else {
                println!(
                    "Redis: {} ({})",
                    url.unwrap_or_default(),
                    if reachable {
                        "reachable"
                    } else {
                        "unreachable"
                    }
                );
            }
        }
        ResponseData::VectorHits(hits) => {
            if hits.is_empty() {
                println!("(no hits)");
            }
            for h in hits {
                println!("#{:<6} score={:<8.4} {}", h.id, h.score, h.payload);
            }
        }
        ResponseData::VectorStatus {
            configured,
            url,
            reachable,
        } => {
            if !configured {
                println!("Qdrant: not configured (set DIVISI_QDRANT_URL to enable)");
            } else {
                println!(
                    "Qdrant: {} ({})",
                    url.unwrap_or_default(),
                    if reachable {
                        "reachable"
                    } else {
                        "unreachable"
                    }
                );
            }
        }
        ResponseData::AgentInstallResult(action) => {
            let mark = if action.executed { "✓" } else { "·" };
            println!(
                "{mark} {:<12} {:?}: {}",
                action.agent, action.action, action.detail
            );
        }
        ResponseData::SetupPlan(plan) => {
            let mode = if plan.dry_run { "dry run" } else { "applied" };
            println!("Setup plan ({mode}):");
            for action in plan.actions {
                let mark = if action.executed { "✓" } else { "·" };
                println!(
                    "  {mark} {:<12} {:?}: {}",
                    action.agent, action.action, action.detail
                );
            }
        }
        ResponseData::IntegrationResult(result) => {
            let mode = if result.dry_run { "dry run" } else { "applied" };
            println!("Integrations ({mode}):");
            for write in result.writes {
                let mark = if write.applied { "✓" } else { "·" };
                println!(
                    "  {mark} {:<12} {} — {}",
                    write.agent, write.config_path, write.detail
                );
                if let Some(backup) = write.backup_path {
                    println!("      backup: {backup}");
                }
            }
        }
        ResponseData::Profiles(profiles) => {
            if profiles.is_empty() {
                println!("(no profiles defined yet — using defaults)");
            }
            for profile in profiles {
                println!("{profile}");
            }
        }
        ResponseData::Session(s) => {
            println!("{}  {}  [{}]", s.id, s.cwd, s.status);
            if !s.title.is_empty() {
                println!("  {}", s.title);
            }
        }
        ResponseData::Sessions(sessions) => {
            if sessions.is_empty() {
                println!("(no sessions)");
            }
            for s in sessions {
                println!("{:<26} {:<8} {}", s.id, s.status, s.title);
            }
        }
        ResponseData::GoalId(id) => println!("{id}"),
        ResponseData::Goals(goals) => {
            if goals.is_empty() {
                println!("(no goals)");
            }
            for g in goals {
                println!(
                    "{:<24} {:<10} {:>2}/{:<2}  {}",
                    g.id, g.status, g.dispatches, g.max_dispatches, g.text
                );
                if g.status == "blocked" {
                    let reason = g.blocked_reason.as_deref().unwrap_or("no reason recorded");
                    println!("{:<24}   -- {reason}", "");
                }
            }
        }
        ResponseData::GoalView(v) => {
            println!("{}  [{}]  {}", v.goal.id, v.goal.status, v.goal.text);
            println!(
                "  dispatches {}/{}  mode {}",
                v.goal.dispatches, v.goal.max_dispatches, v.goal.mode
            );
            if v.total_prompt_tokens > 0 || v.total_completion_tokens > 0 {
                println!(
                    "  tokens ~{} in / ~{} out{}",
                    v.total_prompt_tokens,
                    v.total_completion_tokens,
                    if v.any_tokens_estimated { " (estimated)" } else { "" }
                );
            }
            if let Some(r) = &v.blocked_reason {
                println!("  blocked: {r}");
            }
            if let Some(r) = &v.result_summary {
                println!("  result: {r}");
            }
            if !v.nodes.is_empty() {
                println!("  nodes:");
                for n in &v.nodes {
                    let dep = if n.depends_on.is_empty() {
                        String::new()
                    } else {
                        format!("  <- {}", n.depends_on.join(","))
                    };
                    let tid = n.task_id.map(|t| format!(" #{t}")).unwrap_or_default();
                    println!(
                        "    {:<4} {:<8}/{:<8} {:<9} {}{}{}",
                        n.id, n.kind, n.effort, n.status, n.agent, tid, dep
                    );
                }
            }
            if !v.recent_events.is_empty() {
                println!("  events:");
                for e in &v.recent_events {
                    println!("    {} {:<12} {}", e.ts, e.kind, e.body);
                }
            }
        }
        ResponseData::AgentAuth(rows) => print_agent_auth(&rows),
        ResponseData::Chat(outcome) => {
            for e in outcome.events {
                print_chat_event(&e.kind, &e.body);
            }
        }
        ResponseData::CoordinatorEvents(events) => {
            for e in events {
                println!("{} {:<12} {}", e.ts, e.kind, e.body);
            }
        }
        ResponseData::CoordinatorSnapshot(s) => {
            println!("coordinator — max_parallel {}", s.max_parallel);
            let show = |label: &str, goals: &[divisi_protocol::GoalSummary]| {
                if !goals.is_empty() {
                    println!("  {label}:");
                    for g in goals {
                        println!("    {:<24} {}", g.id, g.text);
                    }
                }
            };
            show("running", &s.running_goals);
            show("queued", &s.queued_goals);
            if !s.blocked_goals.is_empty() {
                println!("  blocked:");
                for g in &s.blocked_goals {
                    let reason = g.blocked_reason.as_deref().unwrap_or("no reason recorded");
                    println!("    {:<24} {} — {reason}", g.id, g.text);
                }
            }
            // E28 spec §8: "waiting: goal_x — nvidia+groq pools spent, resumes ~14:03Z"
            // `capacity_reason` already includes the "resumes ~<eta>" tail
            // (goal::set_waiting_on_capacity's caller composes it once).
            for g in &s.waiting_goals {
                let reason = g.capacity_reason.as_deref().unwrap_or("capacity exhausted, ETA unknown");
                println!("  waiting: {} — {reason}", g.id);
            }
            println!("  pool:");
            for p in s.pool {
                let cap = p.cap.map(|c| c.to_string()).unwrap_or_else(|| "-".into());
                let rl = if p.rate_limited { " rate-limited" } else { "" };
                println!("    {:<20} {}/{}{}", p.agent, p.running, cap, rl);
            }
        }
        ResponseData::PoolStatus(s) => {
            let mode = if s.degraded { "degraded" } else { "normal" };
            println!("pool: {mode} (healthy_ratio={:.2})", s.healthy_ratio);
            if s.benched.is_empty() {
                println!("  no benched keys");
            } else {
                // Rate-limit benches clear on their own within the hour; credit/tier ones mean the key needs
                // payment or a plan (retried after UTC midnight). Shown apart so the count reads right.
                let (paid, temp): (Vec<_>, Vec<_>) = s.benched.iter().partition(|b| b.provenance == "credit" || b.provenance == "tier");
                println!("  benched: {} rate-limited (temporary), {} need payment/plan or serve no such model", temp.len(), paid.len());
                for (title, list) in [("  rate-limited:", &temp), ("  payment/plan/model:", &paid)] {
                    if list.is_empty() {
                        continue;
                    }
                    println!("{title}");
                    for b in list.iter() {
                        println!("    {:<12} {:<20} {:<12} {}s remaining ({})", b.platform, b.model, b.key_id, b.remaining_secs, b.provenance);
                    }
                }
            }
        }
        ResponseData::Empty => {}
        // No dedicated `divisi` subcommand exposes this directly -- it's
        // divisi-notch's own poller preference (E30 Phase 7). Plain debug
        // print is enough for the rare case someone hits it manually.
        ResponseData::NotchSnapshot(snapshot) => println!("{snapshot:#?}"),
        ResponseData::Accounting(result) => {
            if result.events.is_empty() {
                println!("(no usage events recorded yet)");
            } else {
                println!("Usage events:");
                for e in &result.events {
                    println!(
                        "  {} {} {} {} {}:{}:{} in ${:.4} ({}) at {}",
                        e.execution_id,
                        e.trace_id,
                        e.agent,
                        e.provider,
                        e.model,
                        e.prompt_tokens,
                        e.completion_tokens,
                        e.cache_tokens,
                        e.cost_usd,
                        e.occurred_at
                    );
                }
            }
            if !result.breakdowns.is_empty() {
                println!("Token breakdown:");
                for b in &result.breakdowns {
                    println!(
                        "  {} {} {}: {}",
                        b.execution_id, b.trace_id, b.event_type, b.token_count
                    );
                }
            }
            println!(
                "Totals: {} in / {} out / {} cache / total {} tokens / ${:.4}",
                result.totals.prompt_tokens,
                result.totals.completion_tokens,
                result.totals.cache_tokens,
                result.totals.total_tokens,
                result.totals.cost_usd
            );
        }
    }
}

fn print_memory_entry(entry: &divisi_protocol::MemoryEntry) {
    println!("#{}", entry.id);
    println!("  scope:      {:?}", entry.scope);
    println!("  source:     {:?}", entry.source);
    println!("  title:      {}", entry.title);
    println!("  content:    {}", entry.content);
    println!("  confidence: {}", entry.confidence);
    println!("  created:    {}", entry.created_at);
    if let Some(project) = &entry.project {
        println!("  project:    {project}");
    }
    if let Some(agent) = &entry.agent {
        println!("  agent:      {agent}");
    }
    if let Some(task) = &entry.task {
        println!("  task:       {task}");
    }
    if let Some(expires) = &entry.expires_at {
        println!("  expires:    {expires}");
    }
}

fn print_docker_info(info: &divisi_protocol::DockerContainerInfo) {
    let running = match info.running {
        Some(true) => "running",
        Some(false) => "stopped",
        None => "not created",
    };
    let label = match &info.account {
        Some(a) => format!("{}/{a}", info.agent),
        None => info.agent.clone(),
    };
    println!(
        "{:<20} {:<10} {:<24} [{}]",
        label,
        if info.enabled { "enabled" } else { "disabled" },
        info.container_name,
        running
    );
}

fn print_document(doc: &divisi_protocol::DocumentInfo) {
    println!("#{}", doc.id);
    println!("  title:      {}", doc.title);
    println!("  project:    {}", doc.project.as_deref().unwrap_or("-"));
    println!("  source:     {}", doc.source_path);
    println!(
        "  extracted:  {} chars (memory #{})",
        doc.extracted_chars, doc.memory_id
    );
    println!("  ingested:   {}", doc.ingested_at);
}

fn print_task(task: &divisi_protocol::TaskRecord) {
    println!("#{}", task.id);
    println!("  status:      {:?}", task.status);
    println!("  agent:       {}", task.agent);
    println!("  description: {}", task.description);
    if let Some(worktree) = &task.worktree_path {
        println!("  worktree:    {worktree}");
    }
    if let Some(artifact) = &task.artifact_path {
        println!("  artifact:    {artifact}");
    }
    if let Some(code) = task.exit_code {
        println!("  exit code:   {code}");
    }
    if task.timed_out {
        println!("  timed out:   true");
    }
    if let Some(summary) = &task.summary {
        println!("  summary:     {summary}");
    }
    println!("  created:     {}", task.created_at);
    println!("  updated:     {}", task.updated_at);
}

fn print_account_profile(info: &divisi_protocol::AccountProfileInfo) {
    println!("{}/{}", info.agent, info.name);
    println!("  label:    {}", info.label.as_deref().unwrap_or("-"));
    println!("  status:   {}", info.status.as_str());
    println!("  captured: {}", info.captured_at);
    if info.unverified_complete {
        println!("  note:     best-effort capture — not confirmed to cover 100% of this agent's login state");
    }
}

fn print_provider(p: &divisi_protocol::ProviderSpec) {
    println!("{}", p.name);
    println!("  env var:    {}", p.env_var_name);
    println!("  secret:     {} (use `divisi secret get {}` to check, `divisi provider set-key` to change)", p.secret_name, p.secret_name);
    if let Some(url) = &p.base_url {
        println!("  base url:   {url}");
    }
    if !p.models.is_empty() {
        println!("  models:");
        for m in &p.models {
            println!("    {} ({})", m.id, m.name);
        }
    }
}

fn print_kg_entity(e: &divisi_protocol::KgEntity) {
    println!("{}", e.name);
    println!("  type:    {}", e.entity_type);
    println!("  created: {}", e.created_at);
    for o in &e.observations {
        println!("  - {o}");
    }
}

fn install_summary(method: &InstallMethod) -> String {
    match method {
        InstallMethod::Native { detail } => format!("native ({detail})"),
        InstallMethod::StandaloneBinary { detail } => format!("standalone ({detail})"),
        InstallMethod::PackageManager { detail } => format!("package manager ({detail})"),
        InstallMethod::Unsupported { reason } => format!("unsupported ({reason})"),
    }
}

/// A task description can be a full multi-line agent prompt. `divisi task
/// list` is a one-row-per-task table (and gets piped to `grep`), so it
/// shows only the first line, clipped to `max_chars`, with a trailing `…`
/// whenever anything was dropped. `divisi task inspect` still prints the
/// description in full.
fn one_line_description(description: &str, max_chars: usize) -> String {
    let first_line = description.lines().next().unwrap_or("");
    let clipped: String = first_line.chars().take(max_chars).collect();
    let dropped_tail = clipped.chars().count() < first_line.chars().count();
    let dropped_lines = description.trim_end() != first_line;
    if dropped_tail || dropped_lines {
        format!("{clipped}…")
    } else {
        clipped
    }
}

#[cfg(test)]
mod tests {
    use super::one_line_description;

    #[test]
    fn one_line_description_leaves_a_short_single_line_untouched() {
        assert_eq!(one_line_description("add a .gitignore", 80), "add a .gitignore");
    }

    #[test]
    fn one_line_description_marks_a_dropped_second_line() {
        assert_eq!(
            one_line_description("first line\nsecond line", 80),
            "first line…"
        );
    }

    #[test]
    fn one_line_description_ignores_a_bare_trailing_newline() {
        assert_eq!(one_line_description("only line\n", 80), "only line");
    }

    #[test]
    fn one_line_description_clips_an_overlong_first_line() {
        assert_eq!(one_line_description(&"x".repeat(200), 10), format!("{}…", "x".repeat(10)));
    }
}

/// One chat event as a terminal line: `you`, `divisi`, `? ` for a confirmation, `= ` for its result.
pub fn print_chat_event(kind: &str, body: &str) {
    use divisi_protocol::{chat_line, ChatRole};
    let Some(l) = chat_line(kind, body) else { return };
    match l.role {
        ChatRole::You => println!("you     {}", l.text),
        ChatRole::Divisi => println!("divisi  {}{}", l.text, if l.degraded { "  (rules only)" } else { "" }),
        ChatRole::Confirm => println!("?       {}   [divisi chat confirm {} --allow|--deny]", l.text, l.approval_id.unwrap_or_default()),
        ChatRole::Result => println!("=       {}", l.text),
    }
}

/// The auth inventory, grouped the way it is decided: no login needed, needs one and has it
/// (working or out of quota), needs one and lacks it, and everything else.
pub fn print_agent_auth(rows: &[divisi_protocol::AgentAuthRow]) {
    let groups: [(&str, &[&str]); 5] = [
        ("NO AUTH NEEDED (works with no login at all)", &["no_auth_needed"]),
        ("NEEDS AUTH, AUTHENTICATED", &["authed"]),
        ("NEEDS AUTH, AUTHENTICATED BUT OUT OF QUOTA", &["exhausted"]),
        ("NEEDS AUTH, NOT AUTHENTICATED", &["needs_login"]),
        ("OTHER", &["unresponsive", "error", "not_dispatchable", "unverified", "provider", "not_installed"]),
    ];
    for (title, cats) in groups {
        let members: Vec<_> = rows.iter().filter(|r| cats.contains(&r.category.as_str())).collect();
        if members.is_empty() {
            continue;
        }
        println!("{title} ({})", members.len());
        for r in members {
            let mut why = r.evidence.clone();
            if r.category == "exhausted" {
                if let Some(u) = r.until.as_deref().and_then(|u| chrono::DateTime::parse_from_rfc3339(u).ok()) {
                    why = format!("back {}: {}", u.with_timezone(&chrono::Local).format("%a %b %-d %H:%M"), why);
                }
            }
            let tag = if cats.len() > 1 { format!("[{}] ", r.category) } else { String::new() };
            println!("  {:<14} {tag}{}", r.agent, why.chars().take(140).collect::<String>());
        }
        println!();
    }
}

