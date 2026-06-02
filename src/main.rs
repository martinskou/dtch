mod attach;
mod protocol;
mod registry;
mod session;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "A minimal dtach-style pty session runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a command in a named session.
    New {
        /// Session name. Uses /tmp/dtch_<name>.sock.
        name: String,

        /// Number of terminal output lines to replay on attach.
        /// Raw replay can disrupt full-screen terminal programs such as Vim.
        #[arg(short = 'b', long = "buffer-lines", default_value_t = 0)]
        buffer_lines: usize,

        /// Command and arguments to run.
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },

    /// Attach this terminal to a named session.
    #[command(visible_aliases = ["reattach", "retach"])]
    Attach {
        /// Create the session if the socket does not exist.
        #[arg(short = 'c', long = "create")]
        create: bool,

        /// Number of terminal output lines to replay on attach when creating a session.
        /// Raw replay can disrupt full-screen terminal programs such as Vim.
        #[arg(short = 'b', long = "buffer-lines", default_value_t = 0)]
        buffer_lines: usize,

        /// Session name. Uses /tmp/dtch_<name>.sock.
        name: String,

        /// Command and arguments to run when used with `-c`.
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },

    /// List active named sessions.
    List,
}

/// Parses the requested operation and dispatches it to the matching module.
fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::New {
            name,
            buffer_lines,
            command,
        } => {
            print_version();
            session::run_new(name, buffer_lines, command)
        }
        Command::Attach {
            create,
            buffer_lines,
            name,
            command,
        } => {
            print_version();
            attach::run_attach_or_create(name, create, buffer_lines, command)
        }
        Command::List => registry::run_list(),
    }
}

/// Prints the package version embedded from Cargo.toml at build time.
fn print_version() {
    println!("dtch: version {}", env!("CARGO_PKG_VERSION"));
}
