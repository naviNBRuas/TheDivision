pub mod app;
pub mod client;
pub mod ui;

use anyhow::Result;
use app::{App, BackupFlow, InstallFlow, ProviderAddFlow, QuickAddFlow, Tab, TaskAddFlow, TaskDetailFlow, TaskView};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use divisi_core::DivisiDirs;
use std::time::Duration;

/// Runs the divisi dashboard: a tabbed control center over the runtime
/// socket (Agents/Tasks/MCP/Providers/Accounts/Memory/Help), including
/// interactive in-TUI agent-install and provider-add flows.
pub fn run(dirs: DivisiDirs) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(dirs);
    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if app.poll_refresh()
            || app.poll_install()
            || app.poll_provider_add()
            || app.poll_task_add()
            || app.poll_quick_add()
            || app.poll_task_detail()
            || app.poll_backup()
            || app.poll_usage()
            || app.poll_pool()
            || app.poll_provider_key_status()
            || app.poll_goals()
        {
            continue; // redraw immediately on state change
        }

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(app, key.code, key.modifiers) {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Returns true if the app should quit.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    // Modals capture input while open, so typing to confirm/enter text
    // doesn't leak into tab navigation or list selection underneath.
    if !matches!(app.install, InstallFlow::Idle) {
        handle_install_key(app, code);
        return false;
    }
    if !matches!(app.provider_add, ProviderAddFlow::Idle) {
        handle_provider_add_key(app, code);
        return false;
    }
    if !matches!(app.task_add, TaskAddFlow::Idle) {
        handle_task_add_key(app, code);
        return false;
    }
    if !matches!(app.quick_add, QuickAddFlow::Idle) {
        handle_quick_add_key(app, code);
        return false;
    }
    if !matches!(app.task_detail, TaskDetailFlow::Idle) {
        handle_task_detail_key(app, code);
        return false;
    }
    if !matches!(app.backup, BackupFlow::Idle) {
        handle_backup_key(app, code);
        return false;
    }

    // Drilled into one workspace's tasks (Tasks tab): Esc backs out to the
    // workspace list instead of quitting the whole app — checked ahead of
    // the generic `Esc => return true` below, which still applies at the
    // workspace-list level and everywhere else.
    if app.tab == Tab::Tasks && code == KeyCode::Esc && app.task_view != TaskView::Workspaces {
        app.exit_workspace();
        return false;
    }

    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Esc => return true,
        KeyCode::Tab => app.next_tab(),
        KeyCode::BackTab => app.prev_tab(),
        KeyCode::Char('h') if modifiers.contains(KeyModifiers::CONTROL) => app.prev_tab(),
        KeyCode::Char('l') if modifiers.contains(KeyModifiers::CONTROL) => app.next_tab(),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('i') if app.tab == Tab::Agents => app.begin_install(),
        KeyCode::Char('a') if app.tab == Tab::Providers => app.begin_add_provider(),
        KeyCode::Char('a') if matches!(app.tab, Tab::Mcp | Tab::Lsp | Tab::Plugins | Tab::Tools) => app.begin_quick_add(),
        KeyCode::Char('n') if app.tab == Tab::Tasks => app.begin_add_task(),
        KeyCode::Enter if app.tab == Tab::Tasks => app.tasks_enter(),
        KeyCode::Char('d') if matches!(app.tab, Tab::Mcp | Tab::Lsp | Tab::Plugins | Tab::Providers | Tab::Accounts) => app.delete_selected(),
        KeyCode::Char('e') if matches!(app.tab, Tab::Mcp | Tab::Tools) => app.toggle_selected(),
        KeyCode::Char('s') if app.tab == Tab::Plugins => app.sync_selected_plugin(),
        KeyCode::Char('g') if app.tab == Tab::Mcp => app.toggle_mcp_gateway(),
        KeyCode::Char('x') if app.tab == Tab::Backup => app.begin_backup_export(),
        KeyCode::Char('i') if app.tab == Tab::Backup => app.begin_backup_import(),
        _ => {}
    }
    false
}

fn handle_task_detail_key(app: &mut App, code: KeyCode) {
    if matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
        app.cancel_task_detail();
    }
}

fn handle_install_key(app: &mut App, code: KeyCode) {
    match &app.install {
        InstallFlow::Confirming { .. } => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_install(),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.cancel_install(),
            _ => {}
        },
        InstallFlow::Done { .. } | InstallFlow::Failed { .. } => {
            if matches!(code, KeyCode::Enter | KeyCode::Esc) {
                app.cancel_install();
            }
        }
        InstallFlow::Running { .. } | InstallFlow::Idle => {}
    }
}

fn handle_provider_add_key(app: &mut App, code: KeyCode) {
    match &app.provider_add {
        ProviderAddFlow::PickingPreset { .. } => match code {
            KeyCode::Down | KeyCode::Char('j') => app.provider_picker_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.provider_picker_move(-1),
            KeyCode::Enter => app.provider_picker_confirm(),
            KeyCode::Esc => app.cancel_provider_add(),
            _ => {}
        },
        ProviderAddFlow::EnteringKey { .. } => match code {
            KeyCode::Char(c) => app.provider_key_input(c),
            KeyCode::Backspace => app.provider_key_backspace(),
            KeyCode::Enter => app.provider_key_submit(),
            KeyCode::Esc => app.cancel_provider_add(),
            _ => {}
        },
        ProviderAddFlow::Done { .. } | ProviderAddFlow::Failed { .. } => {
            if matches!(code, KeyCode::Enter | KeyCode::Esc) {
                app.cancel_provider_add();
            }
        }
        ProviderAddFlow::Submitting { .. } | ProviderAddFlow::Idle => {}
    }
}

fn handle_task_add_key(app: &mut App, code: KeyCode) {
    match &app.task_add {
        TaskAddFlow::EnteringDescription { .. } => match code {
            KeyCode::Char(c) => app.task_desc_input(c),
            KeyCode::Backspace => app.task_desc_backspace(),
            KeyCode::Enter => app.task_desc_submit(),
            KeyCode::Esc => app.cancel_task_add(),
            _ => {}
        },
        TaskAddFlow::EnteringCwd { .. } => match code {
            KeyCode::Char(c) => app.task_cwd_input(c),
            KeyCode::Backspace => app.task_cwd_backspace(),
            KeyCode::Enter => app.task_cwd_submit(),
            KeyCode::Esc => app.cancel_task_add(),
            _ => {}
        },
        TaskAddFlow::PickingAgents { .. } => match code {
            KeyCode::Down | KeyCode::Char('j') => app.task_agents_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.task_agents_move(-1),
            KeyCode::Char(' ') => app.task_agents_toggle(),
            KeyCode::Char('g') => app.task_toggle_real_home(),
            KeyCode::Enter => app.task_agents_submit(),
            KeyCode::Esc => app.cancel_task_add(),
            _ => {}
        },
        TaskAddFlow::Done { .. } | TaskAddFlow::Failed { .. } => {
            if matches!(code, KeyCode::Enter | KeyCode::Esc) {
                app.cancel_task_add();
            }
        }
        TaskAddFlow::Submitting { .. } | TaskAddFlow::Idle => {}
    }
}

fn handle_quick_add_key(app: &mut App, code: KeyCode) {
    match &app.quick_add {
        QuickAddFlow::EnteringLine { .. } => match code {
            KeyCode::Char(c) => app.quick_add_input(c),
            KeyCode::Backspace => app.quick_add_backspace(),
            KeyCode::Enter => app.quick_add_submit(),
            KeyCode::Esc => app.cancel_quick_add(),
            _ => {}
        },
        QuickAddFlow::Done { .. } | QuickAddFlow::Failed { .. } => {
            if matches!(code, KeyCode::Enter | KeyCode::Esc) {
                app.cancel_quick_add();
            }
        }
        QuickAddFlow::Submitting { .. } | QuickAddFlow::Idle => {}
    }
}

fn handle_backup_key(app: &mut App, code: KeyCode) {
    match &app.backup {
        BackupFlow::EnteringPath { .. } => match code {
            KeyCode::Char(c) => app.backup_path_input(c),
            KeyCode::Backspace => app.backup_path_backspace(),
            KeyCode::Enter => app.backup_path_submit(),
            KeyCode::Esc => app.cancel_backup(),
            _ => {}
        },
        BackupFlow::EnteringPassphrase { .. } | BackupFlow::ConfirmingPassphrase { .. } => match code {
            KeyCode::Char(c) => app.backup_passphrase_input(c),
            KeyCode::Backspace => app.backup_passphrase_backspace(),
            KeyCode::Enter => app.backup_passphrase_submit(),
            KeyCode::Esc => app.cancel_backup(),
            _ => {}
        },
        BackupFlow::Done { .. } | BackupFlow::Failed { .. } => {
            if matches!(code, KeyCode::Enter | KeyCode::Esc) {
                app.cancel_backup();
            }
        }
        BackupFlow::Submitting { .. } | BackupFlow::Idle => {}
    }
}
