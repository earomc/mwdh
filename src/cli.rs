use anyhow::Context;
use clap::{
    Arg, ArgAction, Command, builder::ArgPredicate, crate_authors, crate_description, crate_name, crate_version, value_parser
};

use crate::{Args, CompressionFormat};

pub fn create_cli() -> Command {
    let cli = Command::new(crate_name!())
        .about(crate_description!())
        .author(crate_authors!())
        .version(crate_version!())
        .arg_required_else_help(true)
        .arg(Arg::new("world-path")
            .help("Path to the minecraft server/saves directory that contains /world, /world_nether and /world_the_end")
            .short('w')
            .long("world-path")
            .default_value(".") // current dir
            .num_args(1) // TODO: test if num_args is needed
        )
        .arg(Arg::new("world-name").help("The name of the world directory (or the prefix of the directories in the case of the bukkit world format)").short('N').long("world-name").default_value("world"))
        .arg(Arg::new("include-nether").short('n').long("include-nether").action(ArgAction::SetTrue))
        .arg(Arg::new("include-end").short('e').long("include-end").action(ArgAction::SetTrue))
        .arg(Arg::new("include-overworld").short('o').long("include-overworld").action(ArgAction::SetTrue))
        .arg(Arg::new("bukkit").long("bukkit").action(ArgAction::SetTrue))
        .arg(Arg::new("compression-format").default_value("zstd").short('F').long("compression-format")) // TODO: maybe put compression into one argument
        .arg(Arg::new("compression-level").short('l').long("compression-level").help("For zstd use -7 to 22, for zip use 0 to 9 [defaults: zstd: -7, zip: 6]")
            .default_value_ifs( // sets default values for the compression-level depending on which compression format was specified
                [
                    ("compression-format", ArgPredicate::Equals("zstd".into()), "-7"), // when using zstd, optimizing for speed by default
                    ("compression-format", ArgPredicate::Equals("zip".into()), "6")
                ]
            )
            .value_parser(value_parser!(i8).range(-7..=22)) // zstd compression levels go from -7 to 22
        )
        .arg(Arg::new("threads").short('t').long("threads").default_value("0").help("Number of threads for parallel compression and file serving (0 = auto-detect). Will override compression-threads and server-threads arguments"))
        .arg(Arg::new("compression-threads").long("compression-threads").help("Number of threads for file serving (0 = auto-detect)"))
        .arg(Arg::new("server-threads").long("server-threads").help("Number of threads for parallel compression (0 = auto-detect)"))
        
        // http file serving
        .arg(Arg::new("download-file-name").default_value("world").short('f').long("file-name").help("Specify the downloaded archive's file name - Note: (mwdh will append '.zip' or '.tar.zst' to it)"))
        .arg(Arg::new("bind").long("bind").default_value("0.0.0.0").help("IP address to serve the world download on"))
        .arg(Arg::new("port").short('p').long("port").value_parser(value_parser!(u16).range(1024..=65535)).help("What port to serve the world download on").default_value("3000"))
        .arg(Arg::new("host-path").short('H').long("host-path").default_value("world").help("Host path from where to download the world files"))
        
        /*
        .group(ArgGroup::new("serving").args(["download-file-name", "bind", "port", "host-path"]))
        .group(ArgGroup::new("compressing") // group containing all the args needed to compress a world. not necessary when just serving a world file
            .args(["world-path", "include-nether", "include-end", "include-overworld", "compression-format", "compression-level"]))
         */
        ;
    cli
}

pub fn parse_args(cli: Command) -> anyhow::Result<Args> {
    let matches = cli.get_matches();
    let world_path = matches.get_one::<String>("world-path").unwrap().clone();
    let world_name = matches.get_one::<String>("world-name").unwrap().clone();
    let include_nether = matches.get_flag("include-nether");
    let include_end = matches.get_flag("include-end");
    let include_overworld = matches.get_flag("include-overworld");
    let host_path = matches.get_one::<String>("host-path").unwrap().clone();

    let bind = matches.get_one::<String>("bind").unwrap().clone();
    let port = *matches.get_one::<u16>("port").unwrap();
    let thread_count = matches.get_one::<String>("threads");

    let compression_threads = match matches.get_one::<String>("compression-threads") {
        Some(compression_threads) => compression_threads,
        None => {
            match thread_count {
                Some(thread_count) => thread_count,
                None => "0",
            }
        },
    }.parse::<usize>().context("Expected thread count")?;
    
    let server_threads = match matches.get_one::<String>("server-threads") {
        Some(server_threads) => server_threads,
        None => {
            match thread_count {
                Some(thread_count) => thread_count,
                None => "0",
            }
        },
    }.parse::<usize>().context("Expected thread count")?;
    
    let compression_level = *matches.get_one::<i8>("compression-level").unwrap();
    let compression_format = matches
        .get_one::<String>("compression-format")
        .unwrap()
        .parse::<CompressionFormat>()?;
    let download_file_name = matches
        .get_one::<String>("download-file-name")
        .unwrap()
        .clone();
    let is_bukkit = matches.get_flag("bukkit");

    Ok(Args {
        world_path,
        world_name,
        include_nether,
        include_end,
        include_overworld,
        download_file_name,
        host_path,
        bind,
        port,
        compression_threads,
        server_threads,
        compression_level,
        compression_format,
        is_bukkit,
    })
}
