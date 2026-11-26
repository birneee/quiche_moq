use clap::{CommandFactory, Parser};

fn main() {
    env_logger::builder().filter_level(log::LevelFilter::Info).parse_default_env().init();
    let args = moq_cli::Args::parse();
    if let Err(e) = args.validate() {
        moq_cli::Args::command().error(clap::error::ErrorKind::ArgumentConflict, e).exit();
    }
    moq_cli::node::run(args);
}
