//! `imbo`: terminal commands, the per-computer daemon, and the per-session
//! MCP process, in one binary.

mod adapters;
mod cli;
mod daemon;
mod files;
mod local;
mod mcp;
mod net;
mod paths;
mod procs;
mod render;
mod service;
mod ui;
mod update;

use clap::{Parser, Subcommand};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "imbo",
    version,
    about = "Messages between AI agent sessions, across people and computers.",
    after_help = "Docs: https://imbo.dev"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in, start the background daemon, and connect your AI coding tools.
    Setup(cli::setup::SetupArgs),
    /// Disconnect the tools, stop the daemon and sign this computer out.
    Uninstall(cli::setup::UninstallArgs),
    /// Sign in with an email code.
    Login(cli::login::LoginArgs),
    /// Sign this computer out.
    Logout,
    /// Show your name and account ID.
    Whoami,
    /// Change your name.
    Rename { name: String },
    /// Computers signed in to your account.
    #[command(subcommand)]
    Device(cli::account::DeviceCmd),
    /// Contacts and contact requests.
    #[command(subcommand)]
    Contact(cli::contact::ContactCmd),
    /// Groups.
    #[command(subcommand)]
    Group(cli::group::GroupCmd),
    /// Send a message: imbo send <to> <message> [--file <path>]...
    Send {
        /// name/session, account-ID/session, a group name or #group-ID; a name or account ID alone goes to their terminal inbox.
        to: String,
        /// The message; may be left out when sending files.
        body: Vec<String>,
        /// A file to attach (repeat for more: up to 20, 100 MB together).
        #[arg(long = "file", short = 'f', value_name = "PATH")]
        files: Vec<std::path::PathBuf>,
    },
    /// Download a message's files to ~/.imbo/files/<message-ID>/.
    Get { message_id: i64 },
    /// New terminal-inbox messages (shown once), then messages not yet delivered to your sessions; give an ID to read one.
    Inbox {
        message_id: Option<i64>,
        /// Include delivered messages.
        #[arg(long)]
        all: bool,
    },
    /// Daemon, connection and sessions on this computer.
    Status,
    /// Download and install the latest version, keeping your sign-in and tools.
    Update,
    /// Run the background daemon (started by launchd/systemd).
    #[command(hide = true)]
    Daemon,
    /// Run the MCP server for one agent session (started by the tool).
    #[command(hide = true)]
    Mcp,
    /// Wait for this session's next messages (run in the background by
    /// MiniMax Code and ZCode agents).
    #[command(hide = true)]
    Wait,
    /// Hook entry points: `imbo hook prompt` (Claude Code), `imbo hook <tool> <event>`.
    #[command(hide = true)]
    Hook { args: Vec<String> },
    /// Development helpers.
    #[command(hide = true, subcommand)]
    Dev(cli::dev::DevCmd),
}

fn main() {
    let cli = Cli::parse();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let code = match cli.cmd {
        Cmd::Daemon => run_async(daemon::run()),
        Cmd::Mcp => run_current(mcp::run()),
        Cmd::Hook { args } => run_current(cli::hook::hook(args)),
        cmd => {
            // Output piped into `head` or `grep -q`: end quietly when the reader
            // goes away instead of panicking on the next print (TC-048).
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
            ui::init();
            run_current(cli::dispatch(cmd))
        }
    };
    std::process::exit(code);
}

fn finish(r: anyhow::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            if !e.is::<cli::Reported>() {
                ui::error_line(&format!("{e:#}"));
            }
            1
        }
    }
}

fn run_current(f: impl Future<Output = anyhow::Result<()>>) -> i32 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    finish(rt.block_on(f))
}

fn run_async(f: impl Future<Output = anyhow::Result<()>>) -> i32 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    finish(rt.block_on(f))
}
