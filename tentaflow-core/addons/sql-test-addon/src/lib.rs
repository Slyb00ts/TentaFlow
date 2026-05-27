// =============================================================================
// Plik: addons/sql-test-addon/src/lib.rs
// Opis: Test addon dla F1a M1.W4 SQL API. on_start wykonuje pelny scenariusz:
//   1. sql_exec INSERT z bind params (ochrona przed SQL injection)
//   2. sql_query SELECT — zwraca obie wstawione krotki
//   3. sql_query_one SELECT z WHERE — pierwszy wiersz lub null
//   4. sql_transaction batch INSERT — atomic
//   5. probe DDL via sql_exec — powinno zwrocic Permission (kod 1)
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("sql-test-addon zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("sql-test-addon start — pelen scenariusz F1a M1.W4");

    // 1. INSERT z bind params. Wartosc z apostrofem nie zostanie zinterpretowana.
    let r1 = sql_exec(
        "INSERT INTO items (name, qty, created_at) VALUES (?, ?, ?)",
        &[
            SqlValue::String("alpha'; DROP TABLE items;--".to_string()),
            SqlValue::I64(3),
            SqlValue::I64(1715515200),
        ],
    );
    match r1 {
        Ok(res) => log::info(&format!(
            "sql_exec OK: rows={}, id={}",
            res.rows_affected, res.last_insert_id
        )),
        Err(code) => {
            log::error(&format!("sql_exec FAIL: kod={}", code));
            return code.into();
        }
    }

    // 2. INSERT drugi wiersz.
    let _ = sql_exec(
        "INSERT INTO items (name, qty, created_at) VALUES (?, ?, ?)",
        &[
            SqlValue::String("beta".to_string()),
            SqlValue::I64(5),
            SqlValue::I64(1715515210),
        ],
    );

    // 3. SELECT all.
    match sql_query("SELECT id, name, qty FROM items ORDER BY id", &[]) {
        Ok(rows) => log::info(&format!("sql_query: {} wierszy", rows.len())),
        Err(code) => log::error(&format!("sql_query FAIL: kod={}", code)),
    }

    // 4. SELECT one z WHERE.
    match sql_query_one(
        "SELECT id, qty FROM items WHERE name = ?",
        &[SqlValue::String("beta".to_string())],
    ) {
        Ok(Some(row)) => log::info(&format!(
            "sql_query_one: id={:?} qty={:?}",
            row.get(0).and_then(|v| v.as_i64()),
            row.get(1).and_then(|v| v.as_i64())
        )),
        Ok(None) => log::warn("sql_query_one: brak wiersza"),
        Err(code) => log::error(&format!("sql_query_one FAIL: kod={}", code)),
    }

    // 5. Transaction batch.
    let stmts: Vec<(&str, Vec<SqlValue>)> = vec![
        (
            "INSERT INTO items (name, qty, created_at) VALUES (?, ?, ?)",
            vec![
                SqlValue::String("gamma".to_string()),
                SqlValue::I64(7),
                SqlValue::I64(1715515220),
            ],
        ),
        (
            "UPDATE items SET qty = ? WHERE name = ?",
            vec![SqlValue::I64(99), SqlValue::String("beta".to_string())],
        ),
    ];
    let stmts_ref: Vec<(&str, &[SqlValue])> =
        stmts.iter().map(|(q, p)| (*q, p.as_slice())).collect();
    match sql_transaction(&stmts_ref) {
        Ok(total) => log::info(&format!("sql_transaction OK: rows_affected_total={}", total)),
        Err(code) => log::error(&format!("sql_transaction FAIL: kod={}", code)),
    }

    // 6. DDL probe — powinno failowac z kod=1 (Permission).
    match sql_exec("DROP TABLE items", &[]) {
        Ok(_) => log::error("BLAD: DROP TABLE przeszlo (powinno byc zablokowane)"),
        Err(AbiError::Permission) => log::info("OK: DDL block zadziala (Permission)"),
        Err(code) => log::warn(&format!("DDL probe: nieoczekiwany kod={}", code)),
    }

    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("sql-test-addon stop");
    0
}
