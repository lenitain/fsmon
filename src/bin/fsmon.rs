use anyhow::Result;
use clap::{Parser, Subcommand};
use fsmon::config::{self, Config};
use fsmon::help::{self, HelpTopic};
use fsmon::systemd;

#[derive(Parser)]
#[command(name = "fsmon")]
#[command(author = "fsmon contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "fsmon daemon manager — install, uninstall, and generate instance configuration")]
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

    #[command(about = help::about(HelpTopic::GenerateInstance), long_about = help::long_about(HelpTopic::GenerateInstance))]
    Generate {
        /// Instance name to generate config for (e.g., "web"), creates /etc/fsmon/fsmon-{name}.toml
        #[arg(short, long, required = true, value_name = "NAME")]
        instance: String,

        /// Force overwrite existing config file
        #[arg(short, long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        Commands::Install {
            force,
            protect_system,
            protect_home,
            read_write_paths,
            private_tmp,
        } => {
            let install_cfg = config.install.as_ref();
            let protect_system = protect_system
                .as_deref()
                .or(install_cfg.and_then(|c| c.protect_system.as_deref()));
            let protect_home = protect_home
                .as_deref()
                .or(install_cfg.and_then(|c| c.protect_home.as_deref()));
            let private_tmp = private_tmp
                .as_deref()
                .or(install_cfg.and_then(|c| c.private_tmp.as_deref()));
            let read_write_paths: Option<&[String]> = if read_write_paths.is_empty() {
                install_cfg
                    .and_then(|c| c.read_write_paths.as_ref())
                    .map(|v| v.as_slice())
            } else {
                Some(read_write_paths.as_slice())
            };
            systemd::install(
                force,
                protect_system,
                protect_home,
                read_write_paths,
                private_tmp,
            )?;
        }
        Commands::Uninstall => {
            systemd::uninstall()?;
        }
        Commands::Generate { instance, force } => {
            config::generate_instance_config(&instance, force)?;
        }
    }

    Ok(())
}
