//! One-shot generator: prints the baked tables as Rust source.
//!
//! Run when the game version changes. The output is pasted into `src/tables.rs`, so the runtime
//! never evaluates Lua to learn a board shape or a material score.
use diggle_solver::config::Config;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    let g = &cfg.game_dir;

    let shapes = diggle_solver::game::save::parse_module(&std::fs::read_to_string(
        g.join("items/boardshapes.lua"),
    )?)?;
    println!("// key, boardSize, hexagonal, middleCol, colTileCounts, corners");
    let mut keys: Vec<&String> = shapes.map.keys().collect();
    keys.sort();
    for k in keys {
        let Some(item) = shapes.table_at(k) else { continue };
        let Some(b) = item.table_at("boardData") else { continue };
        let Some(size) = b.table_at("boardSize") else { continue };
        let cols = size.arr[0].as_int().unwrap();
        let rows = size.arr[1].as_int().unwrap();
        let hexa = matches!(b.get("hexagonal"), Some(diggle_solver::game::save::Value::Bool(true)));
        let mid = b.int_at("middleCol").map(|m| m.to_string()).unwrap_or("0".into());
        let ctc: Vec<String> = b
            .table_at("colTileCounts")
            .map(|t| t.arr.iter().filter_map(|v| v.as_int()).map(|n| n.to_string()).collect())
            .unwrap_or_default();
        let corners: Vec<String> = b
            .table_at("corners")
            .map(|t| {
                t.arr
                    .iter()
                    .filter_map(|v| {
                        let t = v.as_table()?;
                        Some(format!("({},{})", t.arr[0].as_int()?, t.arr[1].as_int()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "    Shape {{ key: {k:?}, cols: {cols}, rows: {rows}, hexagonal: {hexa}, middle_col: {mid}, col_tile_counts: &[{}], corners: &[{}] }},",
            ctc.join(","),
            corners.join(",")
        );
    }
    Ok(())
}
