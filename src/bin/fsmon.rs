use anyhow::Result;
use clap::{Parser, Subcommand};
use fsmon::config::Config;
use fsmon::help::{self, HelpTopic};
use fsmon::systemd;

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "fsmon daemon manager — install, uninstall, and generate configuration")]
#[command(after_help = help::daemon_after_help())]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = help::about(HelpTopic::Install), long_about = help::long_about(HelpTopic::Install))]
    Install {
        /// Force overwrite existing service template
        #[arg(short, long)]
        force: bool,

        /// ProtectSystem value (default: strict)
        #[arg(long, value_name = "VALUE")]
        protect_system: Option<String>,

        /// ProtectHome value (default: read-only)
        #[arg(long, value_name = "VALUE")]
        protect_home: Option<String>,

        /// ReadWritePaths value (supports multiple, default: /var/log)
        #[arg(long, value_name = "PATH")]
        read_write_paths: Vec<String>,

        /// PrivateTmp value (default: yes)
        #[arg(long, value_name = "VALUE")]
        private_tmp: Option<String>,
    },

    #[command(about = help::about(HelpTopic::Uninstall), long_about = help::long_about(HelpTopic::Uninstall))]
    Uninstall,

    #[command(about = "Generate default configuration at /etc/fsmon/fsmon.toml")]
    Generate {
        /// Force overwrite existing config file
        #[arg(short, long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let _config = Config::load()?;

    match cli.command {
        Commands::Install {
            force,
            protect_system,
            protect_home,
            read_write_paths,
            private_tmp,
        } => {
            systemd::install(
                force,
                protect_system.as_deref(),
                protect_home.as_deref(),
                if read_write_paths.is_empty() {
                    None
                } else {
                    Some(read_write_paths.as_slice())
                },
                private_tmp.as_deref(),
            )?;
        }
        Commands::Uninstall => {
            systemd::uninstall()?;
        }
        Commands::Generate { force: _ } => {
            Config::generate_default()?;
        }
    }

    Ok(())
}
