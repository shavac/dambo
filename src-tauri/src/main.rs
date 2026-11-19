#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "dambo-ui")]
#[command(author = "kshava", version = "0.1.0", about = "Dambo Danmaku & Dam Controller")]
struct Args {
    #[arg(short, long, default_value = "dambo_config.toml")]
    pub config: String,
}

fn main() {
    let args = Args::parse();
    dambo_ui::run(&args.config);
}
