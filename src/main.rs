//#![feature(hashmap_hasher)] // requires rust-nightly

#[macro_use]
extern crate log;
extern crate time;
extern crate crypto;
extern crate argparse;
extern crate rustc_serialize;
//extern crate twox_hash; // requires rust-nightly
extern crate byteorder;
extern crate rust_base58;

pub mod blockchain;
pub mod common;
#[macro_use]
pub mod callbacks;

use std::fs;
use std::path::{Path, PathBuf};
use std::io::{ErrorKind};
use std::sync::mpsc;
use std::boxed::Box;
use std::process;

use argparse::{ArgumentParser, Store, StoreTrue, List, Print};
use log::LogLevelFilter;


use blockchain::parser::chain;
use blockchain::utils::blkfile::BlkFile;
use blockchain::parser::{ParseMode, BlockchainParser};
use common::SimpleLogger;
use callbacks::Callback;
use callbacks::stats::SimpleStats;
use callbacks::csvdump::CsvDump;


/// Holds all available user arguments
pub struct ParserOptions {
    callback: Box<Callback>,        /* Name of the callback which gets executed for each block. (See callbacks/mod.rs)                      */
    verify_merkle_root: bool,       /* Enable this if you want to check the merkle root of each block. Aborts if something is fishy.        */
    thread_count: u8,               /* Number of core threads. The callback gets sequentially called!                                       */
    resume: bool,                   /* Resumes from latest known hash in chain.json.                                                        */
    new: bool,                      /* Forces new scan                                                                                      */
    blockchain_dir: PathBuf,        /* Path to directory where blk.dat files are stored                                                     */
    chain_storage_path: PathBuf,    /* Path to the longest-chain.json generated by initial header scan                                      */
    worker_backlog: usize,          /* Maximum backlog for each thread. If the backlog is full the worker waits until there is some space.  */
                                    /* Usually this happens if the callback implementation is too slow or if we reached the I/O capabilites */
    verbose: bool,
    debug: bool
}

fn main() {
    let mut log_filter = LogLevelFilter::Info;

    let mut options = parse_args();
    if options.debug {
        log_filter = LogLevelFilter::Trace;
    } else if options.verbose {
        log_filter = LogLevelFilter::Debug;
    }

    SimpleLogger::init(log_filter).expect("Unable to initialize logger!");
    info!(target: "main", "Starting rusty-blockparser-{} ...", env!("CARGO_PKG_VERSION"));

    if options.new {
        fs::remove_file(options.chain_storage_path.clone()).ok();
    }

    // Two iterations possible. First one could be ParseMode::HeaderOnly
    let mut resume = options.resume;
    let iterations = 2;
    for i in 0..iterations {

        // Load chain file into memory
        let chain_file = load_chain_file(options.chain_storage_path.as_path());

        // Determine ParseMode based on existing chain file
        let parse_mode = match chain_file.len() == 0 || resume {
            true => ParseMode::HeaderOnly,
            false => ParseMode::FullData
        };

        // Determine starting location based on previous scans.
        let start_blk_idx = match parse_mode {
            ParseMode::HeaderOnly => 0,
            ParseMode::FullData => chain_file.latest_blk_idx
        };
        let blk_files = BlkFile::from_path(options.blockchain_dir.clone(), start_blk_idx);

        if parse_mode == ParseMode::FullData && chain_file.remaining() == 0 {
            info!("All {} known blocks are processed! Try again with `--resume` to scan for new blocks, or force a full rescan with `--new`", chain_file.get_cur_height());
            process::exit(1);
        }

        {   // Start parser
            let (tx, rx) = mpsc::sync_channel(options.worker_backlog);
            let mut parser = BlockchainParser::new(&mut options,
                parse_mode.clone(), blk_files, chain_file);

            parser.run(tx);
            parser.dispatch(rx);
        }

        info!(target: "main", "Iteration {} finished.", i + 1);

        // If last mode was FullData we can break
        if parse_mode == ParseMode::FullData {
            break;
        }

        // Reset resume mode after first iteration
        if resume {
            resume = false;
        }
    }

    info!(target: "main", "See ya.");
}

/// Initializes all required data
fn load_chain_file(path: &Path) -> chain::ChainStorage {
    let err = match chain::ChainStorage::load(path.clone()) {
        Ok(storage) => return storage,
        Err(e) => e
    };
    match err.kind() {
        ErrorKind::NotFound => {
            info!(target: "init", "No header file found. Generating a new one ...");
            return chain::ChainStorage::default()
        }
        _   => panic!("Couldn't load headers from {}: {}", path.display(), err)
    };
}

/// Parses args or panics if some requirements are not met.
fn parse_args() -> ParserOptions {

    let mut callback_name = String::from("csvdump");
    let mut callback_args = vec!();
    let mut verify_merkle_root = false;
    let mut thread_count = 2;
    let mut resume = false;
    let mut new = false;
    let mut blockchain_dir = String::from("./blocks");
    let mut chain_storage_path = String::from("./chain.json");
    let mut worker_backlog = 100;
    let mut verbose = false;
    let mut debug = false;

    let verify_merkle_str = format!("Verify merkle root (default: {})", &verify_merkle_root);
    let thread_count_str = format!("Thread count (default: {})", &thread_count);
    let blockchain_dir_str = format!("Set blockchain directory (default: {})", &blockchain_dir);
    let chain_file_str = format!("Specify path to chain storage. This is just a internal state file (default: {})", &chain_storage_path);
    let max_work_blog_str = format!("Set maximum worker backlog (default: {})", &worker_backlog);
    {
        let mut ap = ArgumentParser::new();
        ap.set_description("Multithreaded Blockchain Parser written in Rust");
        ap.add_option(&["--list-callbacks"], Print(list_callbacks()), "Lists all available callbacks");
        ap.refer(&mut verify_merkle_root).add_option(&["--verify-merkle-root"], Store, &verify_merkle_str).metavar("BOOL");
        ap.refer(&mut thread_count).add_option(&["-t", "--threads"], Store, &thread_count_str).metavar("COUNT");
        ap.refer(&mut resume).add_option(&["-r", "--resume"], StoreTrue, "Resume from latest known block");
        ap.refer(&mut new).add_option(&["--new"], StoreTrue, "Force complete rescan");
        ap.refer(&mut blockchain_dir).add_option(&["--blockchain-dir"], Store, &blockchain_dir_str).metavar("PATH");
        ap.refer(&mut chain_storage_path).add_option(&["-s", "--chain-storage"], Store, &chain_file_str).metavar("PATH");
        ap.refer(&mut worker_backlog).add_option(&["--backlog"], Store, &max_work_blog_str).metavar("COUNT");
        ap.refer(&mut verbose).add_option(&["-v", "--verbose"], StoreTrue, "Be verbose");
        ap.refer(&mut debug).add_option(&["-d", "--debug"], StoreTrue, "Debug mode");
        ap.add_option(&["--version"], Print(env!("CARGO_PKG_VERSION").to_string()), "Show version");

        ap.refer(&mut callback_name).required().add_argument(&"callback", Store,
                "Set a callback to execute. See `--list-callbacks`");
        ap.refer(&mut callback_args).required().add_argument(&"arguments", List,
                "All following arguments are consumed by this callback.");
        ap.parse_args_or_exit();
    }

    if new && resume {
        println!("Cannot apply `--new` and `--resume` at the same time!");
        process::exit(2);
    }

    callback_args.insert(0, format!("Callback {:?}", callback_name));

    // Add custom callbacks here. Also add them to list_callbacks()
    let callback: Box<Callback> = match callback_name.as_ref() {
        "simplestats"   => Box::new(SimpleStats::parse_args(callback_args)),
        "csvdump"       => Box::new(CsvDump::parse_args(callback_args)),
        cb @ _          => {
            println!("Error: Invalid callback specified: {}", cb);
            process::exit(2);
        }
    };
    ParserOptions {
        callback: callback,
        verify_merkle_root: verify_merkle_root,
        thread_count: thread_count,
        resume: resume,
        new: new,
        blockchain_dir: PathBuf::from(blockchain_dir),
        chain_storage_path: PathBuf::from(chain_storage_path),
        worker_backlog: worker_backlog,
        verbose: verbose,
        debug: debug
    }
}

/// Method to list all available callbacks. TODO: find a better solution
fn list_callbacks() -> String {
    String::from("Available Callbacks:\n\
                  -> csvdump:\tDumps the whole blockchain into CSV files.\n\
                  -> simplestats:\tCallback example. Shows simple Blockchain stats.\n")
}
