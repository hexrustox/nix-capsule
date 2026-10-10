use clap::CommandFactory;
use clap_complete::{Shell, generate};

use nix_capsule::client::cli::Cli as NcapCli;
use nix_capsule::ctl::cli::Cli as NcapCtlCli;

fn main() {
    let mut args = std::env::args().skip(1);
    let bin = args.next().expect("missing BIN argument");
    let shell = match args.next().expect("missing SHELL argument").as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        other => panic!("unknown SHELL `{other}`"),
    };
    let mut cmd = match bin.as_str() {
        "ncap" => NcapCli::command(),
        "ncap-ctl" => NcapCtlCli::command(),
        other => panic!("unknown BIN `{other}`"),
    };
    generate(shell, &mut cmd, bin, &mut std::io::stdout());
}
