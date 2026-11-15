//! Measures enumeration of a real directory through [`LocalFs`].
//!
//! Run with: `cargo run --release -p omni-vfs --example enumerate -- DIR`
//!
//! Run it twice: the first run usually measures a cold disk cache.

use std::time::Instant;

use omni_vfs::{DirEvent, FileSystem, LocalFs};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let dir = std::env::args_os().nth(1).expect("usage: enumerate DIR");
    let start = Instant::now();
    let mut stream = LocalFs.read_dir(dir.as_ref());
    let (mut entries, mut batches, mut first_batch) = (0usize, 0usize, None);
    while let Some(event) = stream.next().await {
        match event {
            DirEvent::Entries(batch) => {
                first_batch.get_or_insert_with(|| start.elapsed());
                entries += batch.len();
                batches += 1;
            }
            DirEvent::Warning(err) => eprintln!("warning: {err}"),
            DirEvent::Done => {}
            DirEvent::Failed(err) => eprintln!("failed: {err}"),
        }
    }
    let total = start.elapsed();
    println!("{entries} entries in {batches} batches");
    if let Some(first) = first_batch {
        println!("first batch after {:.2} ms", first.as_secs_f64() * 1000.0);
    }
    println!("complete after    {:.2} ms", total.as_secs_f64() * 1000.0);
}
