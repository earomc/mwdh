use std::{
    ffi::OsStr,
    path::PathBuf,
    str::FromStr,
};

use anyhow::{Context, Ok, anyhow};
use clap::{
    Arg, ArgAction, ArgMatches, Command, builder::ArgPredicate, crate_authors, crate_description,
    crate_name, crate_version, value_parser,
};

use crate::{ArchiveOptions, CompressionFormat, MwdhOptions, ServerOptions};

pub fn create_cli() -> Command {
    let compress_cmd = Command::new("compress")
        .visible_alias("c")
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
        .arg(Arg::new("compression-threads").long("compression-threads").help("Number of threads for parallel compression (0 = auto-detect)"))
        .arg(Arg::new("file-name").default_value("world").short('f').long("file-name").help("Specify the downloaded archive's file name WITHOUT the file extension - mwdh will append '.zip' or '.tar.zst' to it"));

    let host_cmd = Command::new("host")
        .visible_alias("h")
        .arg(
            Arg::new("bind")
                .long("bind")
                .default_value("0.0.0.0")
                .help("IP address to serve the world download on"),
        )
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .value_parser(value_parser!(u16).range(1024..=65535))
                .help("What port to serve the world download on")
                .default_value("3000"),
        )
        .arg(
            Arg::new("host-path")
                .short('H')
                .long("host-path")
                .default_value("world")
                .help("Host path from where to download the world files"),
        )
        .arg(
            Arg::new("path-to-archive")
                .long("path-to-archive")
                .short('a')
                .help("Specify a path to an archive/world file that you already have ready"),
        )
        .arg(
            Arg::new("server-threads")
                .long("server-threads")
                .help("Number of threads for file serving (0 = auto-detect)"),
        );

    let cmd = Command::new("compress-host")
        .visible_alias("ch")
        .args(compress_cmd.get_arguments())
        .args(host_cmd.get_arguments());

    let cli = Command::new(crate_name!())
        .about(crate_description!())
        .author(crate_authors!())
        .version(crate_version!())
        .arg_required_else_help(true)
        .subcommand(compress_cmd)
        .subcommand(host_cmd)
        .subcommand(cmd);
    cli
}

fn parse_archive_args(matches: &ArgMatches) -> anyhow::Result<ArchiveOptions> {
    let world_path = matches.get_one::<String>("world-path").unwrap().clone();
    let world_name = matches.get_one::<String>("world-name").unwrap().clone();
    let include_nether = matches.get_flag("include-nether");
    let include_end = matches.get_flag("include-end");
    let include_overworld = matches.get_flag("include-overworld");

    if !(include_end || include_nether || include_overworld) {
        return Err(anyhow!("You have to at least include one dimension. Try -o to include the overworld or check out `mwdh help c` for more"))
    }
    
    let thread_count = matches.get_one::<String>("threads");

    let mut compression_threads = match matches.get_one::<String>("compression-threads") {
        Some(compression_threads) => compression_threads,
        None => match thread_count {
            Some(thread_count) => thread_count,
            None => "0",
        },
    }
    .parse::<usize>()
    .context("Expected thread count")?;

    if compression_threads == 0 {
        compression_threads = num_cpus::get();
    }

    let compression_level = *matches.get_one::<i8>("compression-level").unwrap();
    let compression_format = matches
        .get_one::<String>("compression-format")
        .unwrap()
        .parse::<CompressionFormat>()?;
    let archive_name = matches.get_one::<String>("file-name").unwrap().clone();
    let is_bukkit = matches.get_flag("bukkit");

    Ok(ArchiveOptions {
        world_path,
        world_name,
        archive_name,
        include_nether,
        include_end,
        include_overworld,
        threads: compression_threads,
        compression_level,
        compression_format,
        is_bukkit,
        memory_limit_mb: 512, // set to 512MiB for now. TODO: Parse from CLI // TODO: Add this as an option
    })
}

fn parse_archive_host_args(matches: &ArgMatches) -> anyhow::Result<MwdhOptions> {
    Ok(MwdhOptions::Both {
        server: parse_host_args(matches)?,
        archive: parse_archive_args(matches)?,
    })
}

fn parse_host_args(matches: &ArgMatches) -> anyhow::Result<ServerOptions> {
    let host_path = matches.get_one::<String>("host-path").unwrap().clone();
    let bind = matches.get_one::<String>("bind").unwrap().clone();
    let port = *matches.get_one::<u16>("port").unwrap();
    let thread_count = matches.get_one::<String>("threads");
    let path_to_archive = matches.get_one::<String>("path-to-archive");
    let path_to_archive = match path_to_archive {
        Some(path_to_archive) => Some(PathBuf::from_str(&path_to_archive)?),
        None => None,
    };

    let mut server_threads = match matches.get_one::<String>("server-threads") {
        Some(server_threads) => server_threads,
        None => match thread_count {
            Some(thread_count) => thread_count,
            None => "0",
        },
    }
    .parse::<usize>()
    .context("Expected thread count")?;

    if server_threads == 0 {
        server_threads = num_cpus::get();
    }

    Ok(ServerOptions {
        host_path,
        bind,
        port,
        path_to_archive, // FIXME: I dont like this being an Option. Should be initialized differently
        threads: server_threads,
        compression_format: CompressionFormat::TarZstd, // FIXME: i dont like this being a default in this area, because the compressionformat is inferred from the file-ending when just hosting.
    })
}

fn compression_format_from_file_extension(ext: Option<&OsStr>) -> Option<CompressionFormat> {
    ext.and_then(|os_str| os_str.to_str())
        .and_then(|str| match str {
            "zst" => Some(CompressionFormat::TarZstd),
            "zip" => Some(CompressionFormat::ZipDeflate),
            _ => None,
        })
}

pub fn parse_args(cli: Command) -> anyhow::Result<MwdhOptions> {
    let matches = cli.get_matches();
    let options = match matches.subcommand() {
        Some(("compress", matches)) => MwdhOptions::Archive(parse_archive_args(matches)?),
        Some(("host", matches)) => {
            let mut server_options = parse_host_args(matches)?;
            if let Some(ref path_to_archive) = server_options.path_to_archive {
                server_options.compression_format =
                    compression_format_from_file_extension(path_to_archive.extension())
                        .context("Invalid file ending")?;
                return Ok(MwdhOptions::Server(server_options));
            } else {
                return Err(anyhow!(
                    "When just hosting, you need to specify a path to an archive with .zst or .zip ending"
                ));
            }
        }
        Some(("compress-host", matches)) => {
            if let MwdhOptions::Both {
                mut server,
                archive,
            } = parse_archive_host_args(matches)?
            {
                if server.path_to_archive.is_none() {
                    server.path_to_archive = Some(
                        PathBuf::from_str(&archive.archive_name)?
                            .with_extension(archive.compression_format.get_file_ending()),
                    )
                } else {
                    return Err(anyhow!("You cannot specify an archive path when using MWDH to archive. If you want to just host use `mwdh host`, else remove the archive-path argument"))
                }
                return Ok(MwdhOptions::Both { server, archive });
            }
            unreachable!()
        }
        _ => unreachable!("clap should ensure we don't get here"),
    };

    Ok(options)
}
