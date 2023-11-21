use backtrace::Backtrace;
use std::{env, io, panic};

use clap::{Parser, Subcommand};
use colored::Colorize;
use crossterm::{
    event::DisableMouseCapture,
    execute,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
};

use heimdall_cache::{cache, CacheArgs};
use heimdall_common::{
    constants::ADDRESS_REGEX,
    ether::rpc,
    utils::{
        io::{
            file::{write_file, write_lines_to_file},
            logging::Logger,
        },
        version::{current_version, remote_version},
    },
};
use heimdall_config::{config, get_config, ConfigArgs};
use heimdall_core::{
    cfg::{cfg, output::write_cfg_to_file, CFGArgs},
    decode::{decode, DecodeArgs},
    decompile::{decompile, out::abi::ABIStructure, DecompilerArgs},
    disassemble::{disassemble, DisassemblerArgs},
    dump::{dump, DumpArgs},
    snapshot::{snapshot, util::csv::generate_and_write_contract_csv, SnapshotArgs},
};
use tui::{backend::CrosstermBackend, Terminal};

#[derive(Debug, Parser)]
#[clap(name = "heimdall", author = "Jonathan Becker <jonathan@jbecker.dev>", version)]
pub struct Arguments {
    #[clap(subcommand)]
    pub sub: Subcommands,
}

#[derive(Debug, Subcommand)]
#[clap(
    about = "Heimdall is an advanced Ethereum smart contract toolkit for forensic and heuristic analysis.",
    after_help = "For more information, read the wiki: https://jbecker.dev/r/heimdall-rs/wiki"
)]
#[allow(clippy::large_enum_variant)]
pub enum Subcommands {
    #[clap(name = "disassemble", about = "Disassemble EVM bytecode to assembly")]
    Disassemble(DisassemblerArgs),

    #[clap(name = "decompile", about = "Decompile EVM bytecode to Solidity")]
    Decompile(DecompilerArgs),

    #[clap(name = "cfg", about = "Generate a visual control flow graph for EVM bytecode")]
    CFG(CFGArgs),

    #[clap(name = "decode", about = "Decode calldata into readable types")]
    Decode(DecodeArgs),

    #[clap(name = "config", about = "Display and edit the current configuration")]
    Config(ConfigArgs),

    #[clap(name = "cache", about = "Manage heimdall-rs' cached files")]
    Cache(CacheArgs),

    #[clap(name = "dump", about = "Dump the value of all storage slots accessed by a contract")]
    Dump(DumpArgs),
    #[clap(
        name = "snapshot",
        about = "Infer function information from bytecode, including access control, gas
    consumption, storage accesses, event emissions, and more"
    )]
    Snapshot(SnapshotArgs),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse();
    // handle catching panics with
    panic::set_hook(Box::new(|panic_info| {
        // cleanup the terminal (break out of alternate screen, disable mouse capture, and show the
        // cursor)
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).unwrap();
        disable_raw_mode().unwrap();
        execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).unwrap();
        terminal.show_cursor().unwrap();

        // print the panic message
        let backtrace = Backtrace::new();
        let (logger, _) = Logger::new("TRACE");
        logger.fatal(&format!(
            "thread 'main' encountered a fatal error: '{}'!",
            panic_info.to_string().bright_white().on_bright_red().bold(),
        ));
        logger.fatal(&format!("Stack Trace:\n\n{backtrace:#?}"));
    }));

    let configuration = get_config();

    // get the current working directory
    let mut output_path = env::current_dir()?.into_os_string().into_string().unwrap();

    match args.sub {
        Subcommands::Disassemble(mut cmd) => {
            // get specified output path
            output_path.push_str(&format!("/{}", cmd.output));
            
            // if the user has not specified a rpc url, use the default
            if cmd.rpc_url.as_str() == "" {
                cmd.rpc_url = configuration.rpc_url;
            }

            let assembly = disassemble(cmd.clone()).await?;

            if cmd.output == "print" {
                println!("{}", assembly);
            } else {
                let (dir_path, filename) = if ADDRESS_REGEX.is_match(&cmd.target).unwrap() {
                    (format!("{}/{}", output_path, &cmd.target), "disassembled.asm")
                } else {
                    (format!("{}/local", output_path), "disassembled.asm")
                };
            
                std::fs::create_dir_all(&dir_path).expect("Failed to create output directory");
                let full_path = format!("{}/{}", dir_path, filename);
                write_file(&full_path, &assembly);
            }
        }

        Subcommands::Decompile(mut cmd) => {
            // if the user has not specified a rpc url, use the default
            if cmd.rpc_url.as_str() == "" {
                cmd.rpc_url = configuration.rpc_url;
            }

            let result = decompile(cmd.clone()).await?;

            // get specified output path
            output_path.push_str(&format!("/{}", cmd.output));

            if cmd.output == "print" {
                if let Some(abi) = &result.abi {
                    println!("ABI: {}", serde_json::to_string_pretty(abi).unwrap());
                }
                if let Some(source) = &result.source {
                    println!("Source: {}", source);
                }
            } else {
                // write to file
                let (dir_path, abi_filename, solidity_filename, yul_filename) = 
                    if ADDRESS_REGEX.is_match(&cmd.target).unwrap() {
                        let chain_id = rpc::chain_id(&cmd.rpc_url).await.unwrap();
                        (
                            format!("{}/{}/{}", output_path, chain_id, cmd.target),
                            "abi.json",
                            "decompiled.sol",
                            "decompiled.yul"
                        )
                    } else {
                        (
                            format!("{}/local", output_path),
                            "abi.json",
                            "decompiled.sol",
                            "decompiled.yul"
                        )
                    };
        
                std::fs::create_dir_all(&dir_path).expect("Failed to create output directory");
        
                if let Some(abi) = result.abi {
                    // write the ABI to a file
                    let full_path = format!("{}/{}", dir_path, abi_filename);
                    write_file(
                        &full_path,
                        &format!(
                            "[{}]",
                            abi.iter()
                                .map(|x| {
                                    match x {
                                        ABIStructure::Function(x) => {
                                            serde_json::to_string_pretty(x).unwrap()
                                        }
                                        ABIStructure::Error(x) => {
                                            serde_json::to_string_pretty(x).unwrap()
                                        }
                                        ABIStructure::Event(x) => {
                                            serde_json::to_string_pretty(x).unwrap()
                                        }
                                    }
                                })
                                .collect::<Vec<String>>()
                                .join(",\n")
                        ),
                    );
                }
                if let Some(source) = &result.source {
                    let full_path = if cmd.include_solidity {
                        format!("{}/{}", dir_path, solidity_filename)
                    } else {
                        format!("{}/{}", dir_path, yul_filename)
                    };
                    write_file(&full_path, source);
                }
            }
        }

        Subcommands::Decode(mut cmd) => {
            // if the user has not specified a rpc url, use the default
            if cmd.rpc_url.as_str() == "" {
                cmd.rpc_url = configuration.rpc_url;
            }

            // if the user has not specified a openai api key, use the default
            if cmd.openai_api_key.as_str() == "" {
                cmd.openai_api_key = configuration.openai_api_key;
            }

            // set cmd.verbose to 6
            cmd.verbose = clap_verbosity_flag::Verbosity::new(5, 0);

            let _ = decode(cmd).await;
        }

        Subcommands::CFG(mut cmd) => {
            // if the user has not specified a rpc url, use the default
            if cmd.rpc_url.as_str() == "" {
                cmd.rpc_url = configuration.rpc_url;
            }

            let cfg = cfg(cmd.clone()).await?;

            // get specified output path
             output_path.push_str(&format!("/{}", cmd.output));

            // write to file
            let dir_path = if ADDRESS_REGEX.is_match(&cmd.target).unwrap() {
                let chain_id = rpc::chain_id(&cmd.rpc_url).await.unwrap();
                format!("{}/{}/{}", output_path, chain_id, cmd.target)
            } else {
                format!("{}/local", output_path)
            };
        
            std::fs::create_dir_all(&dir_path).expect("Failed to create output directory");
            write_cfg_to_file(&cfg, &cmd, dir_path);
        }

        Subcommands::Dump(mut cmd) => {
            // if the user has not specified a rpc url, use the default
            if cmd.rpc_url.as_str() == "" {
                cmd.rpc_url = configuration.rpc_url;
            }

            // if the user has not specified a transpose api key, use the default
            if cmd.transpose_api_key.as_str() == "" {
                cmd.transpose_api_key = configuration.transpose_api_key;
            }

            let result = dump(cmd.clone()).await?;
            let mut lines = Vec::new();

            // get specified output path
            output_path.push_str(&format!("/{}", cmd.output));


            // add header
            lines.push(String::from("last_modified,alias,slot,decoded_type,value"));

            // add rows
            for row in result {
                lines.push(format!(
                    "{},{},{},{},{}",
                    row.last_modified, row.alias, row.slot, row.decoded_type, row.value
                ));
            }

            if cmd.output == "print" {
                for line in &lines {
                    println!("{}", line);
                }
            } else {
                // write to file
                if ADDRESS_REGEX.is_match(&cmd.target).unwrap() {
                    output_path.push_str(&format!(
                        "/{}/{}/dump.csv",
                        rpc::chain_id(&cmd.rpc_url).await.unwrap(),
                        &cmd.target
                    ));
                } else {
                    output_path.push_str("/local/dump.csv");
                }
                write_lines_to_file(&output_path, lines);
            }
        }

        Subcommands::Snapshot(mut cmd) => {
            // if the user has not specified a rpc url, use the default
            if cmd.rpc_url.as_str() == "" {
                cmd.rpc_url = configuration.rpc_url;
            }

            // write to file
            if ADDRESS_REGEX.is_match(&cmd.target).unwrap() {
                output_path.push_str(&format!(
                    "/{}/{}/snapshot.csv",
                    rpc::chain_id(&cmd.rpc_url).await.unwrap(),
                    &cmd.target,
                ));
            } else {
                output_path.push_str("/local/snapshot.csv");
            }

            let snapshot = snapshot(cmd).await?;
            generate_and_write_contract_csv(
                &snapshot.snapshots,
                &snapshot.resolved_errors,
                &snapshot.resolved_events,
                &output_path,
            )
        }
        Subcommands::Config(cmd) => {
            config(cmd);
        }

        Subcommands::Cache(cmd) => {
            _ = cache(cmd);
        }
    }

    // check if the version is up to date
    let remote_version = remote_version().await;
    let current_version = current_version();

    if remote_version.gt(&current_version) {
        let (logger, _) = Logger::new("TRACE");
        println!();
        logger.info("great news! An update is available!");
        logger
            .info(&format!("you can update now by running: `bifrost --version {remote_version}`"));
    }

    Ok(())
}
