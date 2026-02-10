use crate::args::{Args, Command};
use crate::publish::run_publish;
use crate::subscribe::run_subscribe;
use clap::Parser;
use log::LevelFilter;

mod args;
mod publish;
mod subscribe;

fn main() {
    env_logger::builder().filter_level(LevelFilter::Info).parse_default_env().init();
    let args = Args::parse();

    match args.command {
        Command::Publish(args) => run_publish(&args),
        Command::Subscribe(args) => run_subscribe(&args),
    }
}
