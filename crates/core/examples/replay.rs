//! Replay a recorded fixture through the detector and print every state
//! transition. Handy for eyeballing detector behavior on a new recording
//! before pinning it down in a test.
//!
//! Usage: replay <fixture.ndjson>

use std::path::Path;

use overterm_core::detect::Detector;
use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
use overterm_core::detect::replay::{read_fixture, replay};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: replay <fixture.ndjson>");
    let events = read_fixture(Path::new(&path)).expect("read fixture");
    let mut detector = Detector::new(vec![Box::new(HeuristicAdapter::new(
        HeuristicConfig::default(),
    ))]);
    let changes = replay(&mut detector, &events, 100, 1000);
    println!("{} events, {} transitions", events.len(), changes.len());
    for (t, c) in &changes {
        println!("{t:>7}ms  {:?} -> {:?}  ({:?})", c.from, c.to, c.cause);
    }
}
