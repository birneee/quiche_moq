use clap::Parser;

fn main() {
    env_logger::builder().filter_level(log::LevelFilter::Info).parse_default_env().init();
    moq_cli::node::run(moq_cli::Args::parse());
}
