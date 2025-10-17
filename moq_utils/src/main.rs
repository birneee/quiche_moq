use crate::args::{Args, Command};
use crate::publish::run_publish;
use crate::subscribe::run_subscribe;
use clap::Parser;

mod args;
mod publish;
mod subscribe;
mod h3;

fn main() {
    env_logger::builder().init();
    let args = Args::parse();

    match args.command {
        Command::Publish(args) => run_publish(&args),
        Command::Subscribe(args) => run_subscribe(&args),
    }
}
