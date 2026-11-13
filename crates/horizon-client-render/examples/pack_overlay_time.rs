//! How long a pack selection's overlay takes to build, and to be found built: the steps a
//! start (`startup_root`) and a reload go through before any resource is read, on real folders.
//! `cargo run --profile fast -p horizon-client-render --example pack_overlay_time -- BASE PACKS CACHE ID[,ID...] [ROUNDS]`
//! `BASE` is the root the packs go over (the prepared resources, or the mods' content root),
//! `PACKS` the `resourcepacks` folder, `CACHE` where the overlay is kept, and the ids those of
//! `options.txt` (`file/Name.zip`), bottom first. The first round builds when `CACHE` has no
//! overlay of the selection; the others find it.
use horizon_client_render::packs;
use std::{path::Path, time::Instant};

/// The overlay's own log lines, on standard error.
struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("[{}] {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

fn main() {
    let _ = log::set_logger(&Stderr);
    log::set_max_level(log::LevelFilter::Info);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(base), Some(dir), Some(cache), Some(ids)) =
        (args.first(), args.get(1), args.get(2), args.get(3))
    else {
        eprintln!("usage: pack_overlay_time BASE PACKS CACHE ID[,ID...] [ROUNDS]");
        std::process::exit(2);
    };
    let rounds: usize = args.get(4).and_then(|n| n.parse().ok()).unwrap_or(3);
    let ids: Vec<String> = ids.split(',').map(str::to_owned).collect();
    let started = Instant::now();
    let all = packs::discover(Path::new(base), Path::new(dir));
    println!(
        "discover: {} packs in {} ms",
        all.len(),
        started.elapsed().as_millis()
    );
    let enabled = packs::enabled(&all, &ids);
    if enabled.len() != ids.len() {
        eprintln!("only {} of {} packs found", enabled.len(), ids.len());
        std::process::exit(1);
    }
    for round in 0..rounds {
        let started = Instant::now();
        match packs::resource_root(Path::new(base), &enabled, Path::new(cache)) {
            Ok(root) => println!(
                "resource_root {round}: {} ms ({})",
                started.elapsed().as_millis(),
                root.display()
            ),
            Err(e) => {
                eprintln!("resource_root {round}: {e}");
                std::process::exit(1);
            }
        }
    }
}
