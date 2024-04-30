use clap::{Arg, Command};
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer},
    utill::{setup_logger, ConnectionType},
};
use std::{path::PathBuf, sync::Arc};

fn main() {
    setup_logger();

    let matches = Command::new("Directory Server")
        .version("0.1.0")
        .about("A simple directory server.")
        .subcommand(
            Command::new("start")
                .about("Starts the directory server")
                .arg(
                    Arg::new("data-directory")
                        .long("data-directory")
                        .help("Sets a custom directory for storing server data. Optional.")
                        .value_parser(clap::value_parser!(PathBuf))
                        .default_value("directory.toml"),
                )
                .arg(
                    Arg::new("network")
                        .long("network")
                        .help("Sets the network type for the server. Optional.")
                        .default_value("clearnet"),
                ),
        )
        .get_matches();
    if let Some(sub_matches) = matches.subcommand_matches("start") {
        let data_directory = sub_matches.get_one::<PathBuf>("data-directory").cloned();
        let network = match sub_matches.get_one::<String>("network").map(String::as_str) {
            Some("tor") => Some(ConnectionType::TOR),
            _ => Some(ConnectionType::CLEARNET),
        };

        let directory_server = DirectoryServer::new(data_directory, network).unwrap();
        let arc_directory_server = Arc::new(directory_server);

        start_directory_server(arc_directory_server);
    }
}
