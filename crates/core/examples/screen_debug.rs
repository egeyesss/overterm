//! Show the detection screen model at a point in a recorded fixture.
//! Prints the cursor position and the rows around it, which is the view
//! the heuristic prompt check runs against.
//!
//! Usage: screen_debug <fixture.ndjson> <cols> <rows> <cutoff_ms>

use std::path::Path;

use overterm_core::detect::replay::{Dir, read_fixture};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("fixture path");
    let cols: u16 = args.next().expect("cols").parse().unwrap();
    let rows: u16 = args.next().expect("rows").parse().unwrap();
    let cutoff: u64 = args.next().expect("cutoff ms").parse().unwrap();

    let events = read_fixture(Path::new(&path)).expect("read fixture");
    let mut parser = vt100::Parser::new(rows, cols, 0);
    for ev in &events {
        if ev.dir == Dir::Output && ev.t_ms <= cutoff {
            parser.process(&ev.bytes);
        }
    }

    let screen = parser.screen();
    let (cur_row, cur_col) = screen.cursor_position();
    println!("cursor at row {cur_row}, col {cur_col} (grid {rows}x{cols})");
    for row in 0..rows {
        let mut text = String::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                text.push_str(cell.contents());
            }
        }
        let marker = if row == cur_row { ">>" } else { "  " };
        println!("{marker} {row:>3} {:?}", text.trim_end());
    }
}
