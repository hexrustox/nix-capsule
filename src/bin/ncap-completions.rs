use clap::{CommandFactory, Parser, ValueEnum};
use clap_complete::{Shell, generate};

use nix_capsule::client::cli::Cli as NcapCli;
use nix_capsule::ctl::cli::Cli as NcapCtlCli;

fn main() {
    let cli = Cli::parse();
    let mut cmd = match cli.bin {
        Binary::Ncap => NcapCli::command(),
        Binary::NcapCtl => NcapCtlCli::command(),
    };
    generate(
        cli.shell,
        &mut cmd,
        String::from(cli.bin),
        &mut std::io::stdout(),
    );
}

/// Print the completion script of `bin` for `shell` on stdout
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Binary to describe: `ncap` or `ncap-ctl`
    #[arg()]
    bin: Binary,

    /// Shell to emit: `bash`, `zsh`, `fish`, `elvish`, or `powershell`
    #[arg()]
    shell: Shell,
}

#[derive(ValueEnum, Clone)]
enum Binary {
    Ncap,
    NcapCtl,
}

impl From<Binary> for String {
    fn from(value: Binary) -> Self {
        match value {
            Binary::Ncap => "ncap",
            Binary::NcapCtl => "ncap-ctl",
        }
        .to_string()
    }
}
