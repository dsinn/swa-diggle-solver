//! What does the planner actually decide, from a checkpoint alone?
//!
//! Written on 2026-08-16, after a live diagnosis was built on a stale log line. The run's report
//! printed `door choice: Heart -> l32` on every lap of a ping-pong; the save said Enthorpe's shelf
//! held `healthBuff.stock = 0` and the plan was `CloseAnomaly -> start`. Both statements came from
//! the same program, and nothing on hand could say which described the current step.
//!
//! So: load a checkpoint's save and, optionally, the map cache from the same run, and print the
//! planner's answers with nothing between them and the reader.
//!
//! `spike_heart_probe <checkpoint dir> [map-cache/world-0.txt]`
use diggle_solver::overworld::WorldMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = std::env::args().skip(1);
    let dir = a.next().ok_or("usage: spike_heart_probe <checkpoint> [cache]")?;
    let text = std::fs::read_to_string(std::path::Path::new(&dir).join("mainSaveData"))?;
    let save = diggle_solver::game::save::parse(&text)?;

    let mut m = WorldMap::new();
    if let Some(cache) = a.next() {
        let edges = m.absorb_cache(&std::fs::read_to_string(&cache)?);
        println!("recalled {edges} edges from {cache}");
    }
    m.apply_save(&save);

    println!("standing at {:?}", m.here());
    println!("gold {}  wants_rest {}  wants_a_heart {}", m.gold(), m.wants_rest(), m.wants_a_heart());
    println!("anomaly {:?} open {:?}", m.anomaly().map(|p| p.key.clone()), m.anomaly_is_open());
    println!("plan     {:?}", m.next_target());
    println!("next hop {:?}", m.next_hop());
    Ok(())
}
