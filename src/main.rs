mod cmd;

use clap::{Parser, Subcommand};
use cmd::auth::AuthArgs;
use cmd::bastion::BastionArgs;
use cmd::fleet::DoctorArgs;
use cmd::host::HostArgs;
use cmd::lifecycle::{BuildArgs, FetchArgs, ProjectArgs, TransferArgs};
use cmd::plan::PlanArgs;
use cmd::registry::{PullArgs, PushArgs};
use cmd::transfer::{CpArgs, RmArgs};
use cmd::tunnel::TunnelArgs;
use cmd::watch::WatchArgs;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "binport",
    version,
    about = "Build portable toolboxes and run them on SSH hosts"
)]
struct Cli {
    /// Prompt for an SSH password instead of using keys or an agent
    #[arg(long, global = true)]
    password: bool,

    /// Print platform, cache, and transfer details
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Maximum number of simultaneous Fleet connections
    #[arg(long, global = true, default_value_t = 10)]
    concurrency: usize,

    /// Emit machine-readable JSON
    #[arg(long, global = true)]
    json: bool,

    /// Allocate an interactive terminal for a single remote host
    #[arg(short = 't', long, global = true)]
    tty: bool,

    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Set up and manage passwordless SSH authentication
    Auth(AuthArgs),
    /// Inspect bastion compatibility presets
    Bastion(BastionArgs),
    /// Add and manage reusable SSH hosts and jump routes
    Host(HostArgs),
    /// Resolve Binfile sources into Binport.lock
    Resolve(BuildArgs),
    /// Build a toolbox from a Binfile
    Build(BuildArgs),
    /// List tools in the Binfile or built toolbox
    #[command(alias = "list")]
    Ls(ProjectArgs),
    /// Download tools into the local cache
    Fetch(FetchArgs),
    /// Show toolbox and cache status
    Status(ProjectArgs),
    /// Copy a file between local and remote paths
    Cp(CpArgs),
    /// Remove a remote file or directory
    Rm(RmArgs),
    /// Remove downloaded toolbox cache
    Clean,
    /// Export the built toolbox as one offline file
    Export(TransferArgs),
    /// Load an offline toolbox file
    Load(TransferArgs),
    /// Pack a built toolbox as a local OCI image layout
    Pack(TransferArgs),
    /// Unpack a local OCI image layout into the project
    Unpack(TransferArgs),
    /// Pull an OCI toolbox from a Registry into the project
    Pull(PullArgs),
    /// Push the built toolbox to an OCI Registry
    Push(PushArgs),
    /// Check host reachability, platform, route, latency, and toolbox cache
    Doctor(DoctorArgs),
    /// Preload the complete toolbox onto a host or fleet
    Warm(DoctorArgs),
    /// Show hosts, routes, and artifacts without connecting
    Plan(PlanArgs),
    /// Repeatedly run a toolbox command and report fleet changes
    Watch(WatchArgs),
    /// Forward local ports to remote services through SSH
    Tunnel(TunnelArgs),
    /// Execute a toolbox tool on an SSH host
    #[command(external_subcommand)]
    Remote(Vec<OsString>),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    binport::progress::set_enabled(!cli.json);
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("binport: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> io::Result<u8> {
    let use_password = cli.password;
    let verbose = cli.verbose;
    let concurrency = cli.concurrency;
    let json = cli.json;
    let tty = cli.tty;
    match cli.command {
        CommandKind::Auth(args) => cmd::auth::run(args, json),
        CommandKind::Bastion(args) => cmd::bastion::run(args, use_password, json),
        CommandKind::Host(args) => cmd::host::run(args, use_password, json),
        CommandKind::Resolve(args) => cmd::lifecycle::resolve(args),
        CommandKind::Build(args) => cmd::lifecycle::build(args),
        CommandKind::Ls(args) => cmd::lifecycle::list(args),
        CommandKind::Fetch(args) => cmd::lifecycle::fetch(args),
        CommandKind::Status(args) => cmd::lifecycle::status(args),
        CommandKind::Cp(args) => cmd::transfer::copy(args, use_password, json),
        CommandKind::Rm(args) => cmd::transfer::remove(args, use_password, json),
        CommandKind::Clean => cmd::lifecycle::clean(),
        CommandKind::Export(args) => cmd::lifecycle::export(args),
        CommandKind::Load(args) => cmd::lifecycle::load(args),
        CommandKind::Pack(args) => cmd::lifecycle::pack(args),
        CommandKind::Unpack(args) => cmd::lifecycle::unpack(args),
        CommandKind::Pull(args) => cmd::registry::pull(args),
        CommandKind::Push(args) => cmd::registry::push(args),
        CommandKind::Doctor(args) => cmd::fleet::doctor(args, use_password, concurrency, json),
        CommandKind::Warm(args) => cmd::fleet::warm(args, use_password, concurrency, json),
        CommandKind::Plan(args) => cmd::plan::run(args, json),
        CommandKind::Watch(args) => cmd::watch::run(args, use_password, concurrency, json),
        CommandKind::Tunnel(args) => cmd::tunnel::run(args, use_password),
        CommandKind::Remote(args) => {
            cmd::remote::run(args, use_password, verbose, concurrency, json, tty)
        }
    }
}
