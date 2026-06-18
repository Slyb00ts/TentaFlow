// ===== File: examples/dump_flows.rs — diagnostic: run migrations+seed on a DB
// path and print the resulting flow list (id tail, name, node/edge counts).
// Usage: cargo run --example dump_flows -- <db_path> =====

use std::path::Path;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dump_flows <db_path>");
    let pool = tentaflow_core::db::init(Path::new(&path)).expect("db init (migrate + seed)");
    let conn = pool.lock().unwrap();

    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .unwrap();
    println!("migration version: {version}\n");

    let mut stmt = conn
        .prepare(
            "SELECT substr(id,-4), name, \
             json_array_length(json_extract(flow_json,'$.nodes')), \
             json_array_length(json_extract(flow_json,'$.edges')) \
             FROM flows ORDER BY name",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap();
    println!("{:<6} | {:<18} | nodes | edges", "id", "name");
    println!("{:-<6}-+-{:-<18}-+-------+------", "", "");
    for row in rows {
        let (id, name, nodes, edges) = row.unwrap();
        println!("{id:<6} | {name:<18} | {nodes:^5} | {edges:^5}");
    }
}
