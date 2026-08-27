//! Replay a recorded fixture through the detector and print every state
//! transition. Handy for eyeballing detector behavior on a new recording
//! before pinning it down in a test.
//!
//! Usage: replay <fixture.ndjson> [cols rows]

use std::path::Path;

use overterm_core::detect::Detector;
use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
use overterm_core::detect::hook::HookAdapter;
use overterm_core::detect::replay::{read_fixture, replay};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: replay <fixture.ndjson> [cols rows]");
    let cols: u16 = args.next().map_or(100, |a| a.parse().expect("bad cols"));
    let rows: u16 = args.next().map_or(30, |a| a.parse().expect("bad rows"));
    let events = read_fixture(Path::new(&path)).expect("read fixture");
    // The same adapters, in the same order, a live session runs.
    let mut detector = Detector::new(vec![
        Box::new(HookAdapter::new()),
        Box::new(HeuristicAdapter::new(HeuristicConfig {
            cols,
            rows,
            ..Default::default()
        })),
    ]);
    let changes = replay(&mut detector, &events, 100, 1000);
    println!("{} events, {} transitions", events.len(), changes.len());
    for (t, c) in &changes {
        println!("{t:>7}ms  {:?} -> {:?}  ({:?})", c.from, c.to, c.cause);
    }
}
