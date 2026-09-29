//! Imports a file twice (cold, then from cache) and prints timings.
//! Usage: cargo run --release -p oddech-media --example import_bench -- <file> [cache_dir]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("usage: import_bench <file> [cache_dir]"));
    let cache = args.next().map(PathBuf::from).unwrap_or_else(oddech_core::cache::default_cache_root);
    for round in ["first", "second"] {
        let t = Instant::now();
        let progress = Arc::new(oddech_media::ImportProgress::default());
        let media = oddech_media::import_media(&path, &cache, &progress).expect("import");
        println!("{round}: {:.2?} (from_cache={})", t.elapsed(), media.from_cache);
        if round == "first" {
            for tr in &media.tracks {
                println!(
                    "  {} {:?} {} ch @ {} Hz, {:.2} s, pyramid {} KiB",
                    tr.info.id,
                    tr.info.display_name(),
                    tr.info.channels,
                    tr.info.sample_rate,
                    tr.store.duration_secs(),
                    tr.pyramid.memory_bytes() / 1024
                );
            }
        }
    }
}
