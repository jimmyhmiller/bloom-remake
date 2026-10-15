//! Inspect recorded tick times without evaluating the program or printing payloads/seeds.
#![allow(clippy::print_stdout, clippy::print_stderr)]
use blossom_trace::node::{NodeRecord, TraceReader};
use std::{fs::File, io::BufReader};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("usage: timeline TRACE")?;
    if args.next().is_some() {
        return Err("usage: timeline TRACE".into());
    }
    let mut trace = TraceReader::open(BufReader::new(File::open(path)?))?;
    println!("tick,now_ns,events,delivered,ingress");
    while let Some(record) = trace.next_record()? {
        if let NodeRecord::Tick {
            tick,
            now,
            events,
            delivered,
            ingress,
        } = record
        {
            println!(
                "{},{},{},{},{}",
                tick.0,
                now.0,
                events.len(),
                delivered.len(),
                ingress.len()
            );
        }
    }
    eprintln!("torn_tail={}", trace.torn());
    Ok(())
}
