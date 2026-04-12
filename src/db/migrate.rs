//! Bidirectional MySQL ↔ PostgreSQL database migration.
//!
//! Streams rows in chunks of `CHUNK_SIZE` — memory usage is constant
//! regardless of table size, so it works where Python tools OOM.
//!
//! Usage: `dip db migrate --from <mysql-service> --to <postgres-service>`

use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use colored::Colorize;
use futures::TryStreamExt;
use indicatif::ProgressBar;
use rust_decimal::Decimal;
use sqlx::Row;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgPool, PgSslMode};

use crate::db::{DbConfig, detect_by_labels};
use crate::project::ProjectConfig;
use crate::utils::output::{Output, make_spinner};

const CHUNK_SIZE: usize = 500;

// ─── public entry point ───────────────────────────────────────────────────────

pub fn run_migrate(
    project: &ProjectConfig,
    from: &str,
    to: &str,
    tables: Option<&str>,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    let out = Output::new(no_color);

    let mut services = detect_by_labels(project, verbose)?;
    if services.is_empty() {
        anyhow::bail!(
            "No database services found — add `dip.db: mysql` or `dip.db: postgres` \
             labels to your docker-compose.yml"
        );
    }

    let from_idx = services
        .iter()
        .position(|s| s.service_name == from)
        .ok_or_else(|| {
            let names: Vec<_> = services.iter().map(|s| s.service_name.as_str()).collect();
            anyhow::anyhow!(
                "Service '{}' not found. Available: {}\n  Run `dip db list` to see all services.",
                from,
                names.join(", ")
            )
        })?;

    let to_idx = services
        .iter()
        .position(|s| s.service_name == to)
        .ok_or_else(|| {
            let names: Vec<_> = services.iter().map(|s| s.service_name.as_str()).collect();
            anyhow::anyhow!(
                "Service '{}' not found. Available: {}",
                to,
                names.join(", ")
            )
        })?;

    if from_idx == to_idx {
        anyhow::bail!("--from and --to must be different services");
    }

    // Remove in reverse index order to preserve the lower index
    let (from_svc, to_svc) = if from_idx > to_idx {
        let f = services.swap_remove(from_idx);
        let t = services.swap_remove(to_idx);
        (f, t)
    } else {
        let t = services.swap_remove(to_idx);
        let f = services.swap_remove(from_idx);
        (f, t)
    };

    let from_type = from_svc.backend.name().to_string();
    let to_type = to_svc.backend.name().to_string();
    let from_ip = get_container_ip(&from_svc.container_id)?;
    let to_ip = get_container_ip(&to_svc.container_id)?;
    let from_db = from_svc.config.db_name.clone();

    let tables_filter: Option<Vec<String>> =
        tables.map(|t| t.split(',').map(|s| s.trim().to_string()).collect());

    out.info(&format!(
        "Migrating {} ({}) → {} ({})",
        from_svc.service_name.cyan(),
        from_type.yellow(),
        to_svc.service_name.cyan(),
        to_type.yellow(),
    ));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match (from_type.as_str(), to_type.as_str()) {
            ("mysql", "postgres") => {
                let src = connect_mysql(&from_svc.config, &from_ip).await?;
                let dst = connect_pg(&to_svc.config, &to_ip).await?;
                migrate_mysql_to_pg(src, dst, &from_db, tables_filter.as_deref(), &out, verbose)
                    .await
            }
            ("postgres", "mysql") => {
                let src = connect_pg(&from_svc.config, &from_ip).await?;
                let dst = connect_mysql(&to_svc.config, &to_ip).await?;
                migrate_pg_to_mysql(src, dst, tables_filter.as_deref(), &out, verbose).await
            }
            (a, b) => anyhow::bail!(
                "Unsupported direction: {} → {} (only mysql↔postgres supported)",
                a,
                b
            ),
        }
    })
}

// ─── MySQL → PostgreSQL ───────────────────────────────────────────────────────

async fn migrate_mysql_to_pg(
    src: MySqlPool,
    dst: PgPool,
    src_db: &str,
    filter: Option<&[String]>,
    out: &Output,
    verbose: bool,
) -> Result<()> {
    let tables = mysql_get_tables(&src, src_db, filter).await?;
    if tables.is_empty() {
        out.warning("No tables found in source database");
        return Ok(());
    }
    out.info(&format!("Found {} table(s)", tables.len()));

    type TableSchema = (
        String,
        Vec<MysqlCol>,
        Vec<String>,
        Vec<MysqlIdx>,
        Vec<MysqlFk>,
    );

    // Introspect all tables
    let mut schemas: Vec<TableSchema> = Vec::with_capacity(tables.len());
    for table in &tables {
        let cols = mysql_get_columns(&src, src_db, table).await?;
        let pk = mysql_get_pk(&src, src_db, table).await?;
        let idxs = mysql_get_indexes(&src, src_db, table).await?;
        let fks = mysql_get_fks(&src, src_db, table).await?;
        schemas.push((table.clone(), cols, pk, idxs, fks));
    }

    // Create tables (no FK constraints yet — added after data)
    for (table, cols, pk, _, _) in &schemas {
        let ddl = build_pg_create_table(table, cols, pk);
        if verbose {
            eprintln!("  DDL: {ddl}");
        }
        sqlx::query(&ddl)
            .execute(&dst)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create table \"{table}\": {e}"))?;
    }

    // Stream data table by table
    let mut total_rows = 0u64;
    let start = std::time::Instant::now();

    for (table, cols, _, _, _) in &schemas {
        let pb = make_spinner(table);
        let count = transfer_mysql_to_pg(&src, &dst, table, cols, &pb)
            .await
            .inspect_err(|_| pb.abandon())?;
        pb.finish_with_message(format!("{} — {} rows", table.green(), fmt_num(count)));
        total_rows += count;
    }

    // Add indexes
    for (table, _, _, idxs, _) in &schemas {
        for idx in idxs {
            let ddl = build_pg_index(table, idx);
            if let Err(e) = sqlx::query(&ddl).execute(&dst).await
                && verbose
            {
                eprintln!("  [warn] index on {table}: {e}");
            }
        }
    }

    // Reset sequences for serial/auto-increment columns
    for (table, cols, _, _, _) in &schemas {
        for col in cols {
            if col.is_auto_increment {
                let sql = format!(
                    "SELECT setval(\
                       pg_get_serial_sequence('\"{table}\"', '{col}'), \
                       COALESCE(MAX(\"{col}\"), 1)) FROM \"{table}\"",
                    col = col.name
                );
                sqlx::query(&sql).execute(&dst).await.ok();
            }
        }
    }

    // Add foreign key constraints
    for (table, _, _, _, fks) in &schemas {
        for fk in fks {
            let ddl = build_pg_fk(table, fk);
            if let Err(e) = sqlx::query(&ddl).execute(&dst).await
                && verbose
            {
                eprintln!("  [warn] FK on {table}: {e}");
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    out.success(&format!(
        "Done: {} rows, {} tables, {:.1}s",
        fmt_num(total_rows),
        tables.len(),
        elapsed
    ));
    Ok(())
}

// ─── PostgreSQL → MySQL ───────────────────────────────────────────────────────

async fn migrate_pg_to_mysql(
    src: PgPool,
    dst: MySqlPool,
    filter: Option<&[String]>,
    out: &Output,
    verbose: bool,
) -> Result<()> {
    let tables = pg_get_tables(&src, filter).await?;
    if tables.is_empty() {
        out.warning("No tables found in source database");
        return Ok(());
    }
    out.info(&format!("Found {} table(s)", tables.len()));

    let mut schemas: Vec<(String, Vec<PgCol>, Vec<String>)> = Vec::with_capacity(tables.len());
    for table in &tables {
        let cols = pg_get_columns(&src, table).await?;
        let pk = pg_get_pk(&src, table).await?;
        schemas.push((table.clone(), cols, pk));
    }

    // Create tables
    for (table, cols, pk) in &schemas {
        let ddl = build_mysql_create_table(table, cols, pk);
        if verbose {
            eprintln!("  DDL: {ddl}");
        }
        sqlx::query(&ddl)
            .execute(&dst)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create table `{table}`: {e}"))?;
    }

    // Stream data
    let mut total_rows = 0u64;
    let start = std::time::Instant::now();

    for (table, cols, _) in &schemas {
        let pb = make_spinner(table);
        let count = transfer_pg_to_mysql(&src, &dst, table, cols, &pb)
            .await
            .inspect_err(|_| pb.abandon())?;
        pb.finish_with_message(format!("{} — {} rows", table.green(), fmt_num(count)));
        total_rows += count;
    }

    if verbose {
        out.info(
            "Note: foreign key constraints not migrated automatically (add manually if needed)",
        );
    }

    let elapsed = start.elapsed().as_secs_f64();
    out.success(&format!(
        "Done: {} rows, {} tables, {:.1}s",
        fmt_num(total_rows),
        tables.len(),
        elapsed
    ));
    Ok(())
}

// ─── MySQL schema introspection ───────────────────────────────────────────────

#[derive(Debug)]
struct MysqlCol {
    name: String,
    column_type: String, // raw type: "varchar(255)", "tinyint(1)", "int unsigned"
    is_nullable: bool,
    is_auto_increment: bool,
}

#[derive(Debug)]
struct MysqlIdx {
    name: String,
    columns: Vec<String>,
    is_unique: bool,
}

#[derive(Debug)]
struct MysqlFk {
    name: String,
    column: String,
    ref_table: String,
    ref_column: String,
}

async fn mysql_get_tables(
    pool: &MySqlPool,
    db: &str,
    filter: Option<&[String]>,
) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME",
    )
    .bind(db)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| r.try_get::<String, _>(0).unwrap_or_default())
        .filter(|t| match filter {
            Some(f) => f.iter().any(|x| x == t),
            None => true,
        })
        .collect())
}

async fn mysql_get_columns(pool: &MySqlPool, db: &str, table: &str) -> Result<Vec<MysqlCol>> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, EXTRA, COLUMN_KEY \
         FROM INFORMATION_SCHEMA.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let extra: String = r.try_get("EXTRA").unwrap_or_default();
            let _col_key: String = r.try_get("COLUMN_KEY").unwrap_or_default();
            let nullable: String = r.try_get("IS_NULLABLE").unwrap_or_default();
            MysqlCol {
                name: r.try_get("COLUMN_NAME").unwrap_or_default(),
                column_type: r.try_get("COLUMN_TYPE").unwrap_or_default(),
                is_nullable: nullable.eq_ignore_ascii_case("YES"),
                is_auto_increment: extra.to_lowercase().contains("auto_increment"),
            }
        })
        .collect())
}

async fn mysql_get_pk(pool: &MySqlPool, db: &str, table: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME FROM INFORMATION_SCHEMA.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND COLUMN_KEY = 'PRI' \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| r.try_get::<String, _>(0).unwrap_or_default())
        .collect())
}

async fn mysql_get_indexes(pool: &MySqlPool, db: &str, table: &str) -> Result<Vec<MysqlIdx>> {
    let rows = sqlx::query(
        "SELECT INDEX_NAME, COLUMN_NAME, NON_UNIQUE, SEQ_IN_INDEX \
         FROM INFORMATION_SCHEMA.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_NAME != 'PRIMARY' \
         ORDER BY INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut indexes: Vec<MysqlIdx> = vec![];
    for row in &rows {
        let name: String = row.try_get("INDEX_NAME").unwrap_or_default();
        let col: String = row.try_get("COLUMN_NAME").unwrap_or_default();
        let non_unique: i64 = row.try_get::<i64, _>("NON_UNIQUE").unwrap_or(1);
        if let Some(idx) = indexes.iter_mut().find(|i| i.name == name) {
            idx.columns.push(col);
        } else {
            indexes.push(MysqlIdx {
                name,
                columns: vec![col],
                is_unique: non_unique == 0,
            });
        }
    }
    Ok(indexes)
}

async fn mysql_get_fks(pool: &MySqlPool, db: &str, table: &str) -> Result<Vec<MysqlFk>> {
    let rows = sqlx::query(
        "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
         FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| MysqlFk {
            name: r.try_get("CONSTRAINT_NAME").unwrap_or_default(),
            column: r.try_get("COLUMN_NAME").unwrap_or_default(),
            ref_table: r.try_get("REFERENCED_TABLE_NAME").unwrap_or_default(),
            ref_column: r.try_get("REFERENCED_COLUMN_NAME").unwrap_or_default(),
        })
        .collect())
}

// ─── PostgreSQL schema introspection ─────────────────────────────────────────

#[derive(Debug)]
struct PgCol {
    name: String,
    data_type: String, // from information_schema: "integer", "character varying", etc.
    char_max_len: Option<i32>,
    numeric_precision: Option<i32>,
    numeric_scale: Option<i32>,
    is_nullable: bool,
    is_serial: bool,
}

async fn pg_get_tables(pool: &PgPool, filter: Option<&[String]>) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' ORDER BY table_name",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| r.try_get::<String, _>(0).unwrap_or_default())
        .filter(|t| match filter {
            Some(f) => f.iter().any(|x| x == t),
            None => true,
        })
        .collect())
}

async fn pg_get_pk(pool: &PgPool, table: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT kcu.column_name \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON tc.constraint_name = kcu.constraint_name \
          AND tc.table_schema = kcu.table_schema \
         WHERE tc.table_schema = 'public' AND tc.table_name = $1 \
           AND tc.constraint_type = 'PRIMARY KEY' \
         ORDER BY kcu.ordinal_position",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| r.try_get::<String, _>(0).unwrap_or_default())
        .collect())
}

async fn pg_get_columns(pool: &PgPool, table: &str) -> Result<Vec<PgCol>> {
    let rows = sqlx::query(
        "SELECT column_name, data_type, character_maximum_length, \
                numeric_precision, numeric_scale, is_nullable, column_default \
         FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = $1 \
         ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let name: String = r.try_get("column_name").unwrap_or_default();
            let nullable: String = r.try_get("is_nullable").unwrap_or_default();
            let default: Option<String> = r
                .try_get::<Option<String>, _>("column_default")
                .unwrap_or(None);
            let is_serial = default
                .as_deref()
                .map(|d| d.contains("nextval("))
                .unwrap_or(false);
            PgCol {
                name,
                data_type: r.try_get("data_type").unwrap_or_default(),
                char_max_len: r
                    .try_get::<Option<i32>, _>("character_maximum_length")
                    .unwrap_or(None),
                numeric_precision: r
                    .try_get::<Option<i32>, _>("numeric_precision")
                    .unwrap_or(None),
                numeric_scale: r.try_get::<Option<i32>, _>("numeric_scale").unwrap_or(None),
                is_nullable: nullable.eq_ignore_ascii_case("YES"),
                is_serial,
            }
        })
        .collect())
}

// ─── DDL builders ─────────────────────────────────────────────────────────────

fn mysql_type_to_pg(col: &MysqlCol) -> String {
    let ct = col.column_type.as_str();

    // Boolean special case
    if ct == "tinyint(1)" {
        return "BOOLEAN".to_string();
    }

    // Auto-increment → serial
    if col.is_auto_increment {
        let base = ct.split('(').next().unwrap_or("").trim().to_lowercase();
        return if base.contains("bigint") {
            "BIGSERIAL".to_string()
        } else {
            "SERIAL".to_string()
        };
    }

    let without_paren = ct.find('(').map(|i| &ct[..i]).unwrap_or(ct);
    let base = without_paren
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let unsigned = ct.contains("unsigned");

    // Preserve (p,s) for decimal / bit / char / varchar
    let paren_part: Option<&str> = ct.find('(').zip(ct.find(')')).map(|(s, e)| &ct[s..=e]);

    match base.as_str() {
        "tinyint" | "smallint" => "SMALLINT".to_string(),
        "mediumint" | "int" | "integer" if unsigned => "BIGINT".to_string(),
        "mediumint" | "int" | "integer" => "INTEGER".to_string(),
        "bigint" if unsigned => "NUMERIC(20,0)".to_string(),
        "bigint" => "BIGINT".to_string(),
        "float" => "REAL".to_string(),
        "double" | "real" => "DOUBLE PRECISION".to_string(),
        "decimal" | "numeric" => paren_part
            .map(|p| format!("NUMERIC{p}"))
            .unwrap_or_else(|| "NUMERIC".to_string()),
        "char" => paren_part
            .map(|p| format!("CHAR{p}"))
            .unwrap_or_else(|| "CHAR".to_string()),
        "varchar" => paren_part
            .map(|p| format!("VARCHAR{p}"))
            .unwrap_or_else(|| "TEXT".to_string()),
        "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => "TEXT".to_string(),
        "tinyblob" | "blob" | "mediumblob" | "longblob" | "binary" | "varbinary" => {
            "BYTEA".to_string()
        }
        "bit" => paren_part
            .map(|p| format!("BIT{p}"))
            .unwrap_or_else(|| "BIT".to_string()),
        "date" => "DATE".to_string(),
        "time" => "TIME".to_string(),
        "datetime" => "TIMESTAMP".to_string(),
        "timestamp" => "TIMESTAMPTZ".to_string(),
        "year" => "SMALLINT".to_string(),
        "json" => "JSONB".to_string(),
        _ => "TEXT".to_string(),
    }
}

fn pg_type_to_mysql(col: &PgCol) -> String {
    if col.is_serial {
        return match col.data_type.as_str() {
            "bigint" => "BIGINT AUTO_INCREMENT".to_string(),
            _ => "INT AUTO_INCREMENT".to_string(),
        };
    }

    match col.data_type.as_str() {
        "boolean" => "TINYINT(1)".to_string(),
        "smallint" => "SMALLINT".to_string(),
        "integer" | "int" => "INT".to_string(),
        "bigint" => "BIGINT".to_string(),
        "real" => "FLOAT".to_string(),
        "double precision" => "DOUBLE".to_string(),
        "numeric" | "decimal" => match (col.numeric_precision, col.numeric_scale) {
            (Some(p), Some(s)) => format!("DECIMAL({p},{s})"),
            (Some(p), None) => format!("DECIMAL({p})"),
            _ => "DECIMAL".to_string(),
        },
        "character" | "char" => col
            .char_max_len
            .map(|n| format!("CHAR({n})"))
            .unwrap_or_else(|| "CHAR(1)".to_string()),
        "character varying" => col
            .char_max_len
            .map(|n| format!("VARCHAR({n})"))
            .unwrap_or_else(|| "LONGTEXT".to_string()),
        "text" => "LONGTEXT".to_string(),
        "bytea" => "LONGBLOB".to_string(),
        "date" => "DATE".to_string(),
        "time without time zone" | "time with time zone" => "TIME".to_string(),
        "timestamp without time zone" => "DATETIME".to_string(),
        "timestamp with time zone" => "DATETIME".to_string(), // TZ info dropped
        "json" | "jsonb" => "JSON".to_string(),
        "uuid" => "CHAR(36)".to_string(),
        "bit" | "bit varying" => "BIT(8)".to_string(),
        "ARRAY" => "LONGTEXT".to_string(), // JSON-encoded
        _ => "LONGTEXT".to_string(),
    }
}

fn build_pg_create_table(table: &str, cols: &[MysqlCol], pk: &[String]) -> String {
    let mut lines = vec![];
    for col in cols {
        let pg_type = mysql_type_to_pg(col);
        let null_part = if col.is_nullable { "" } else { " NOT NULL" };
        lines.push(format!("  \"{}\" {}{}", col.name, pg_type, null_part));
    }
    if !pk.is_empty() {
        let pk_cols = pk
            .iter()
            .map(|c| format!("\"{}\"", c))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("  PRIMARY KEY ({pk_cols})"));
    }
    format!(
        "CREATE TABLE IF NOT EXISTS \"{}\" (\n{}\n)",
        table,
        lines.join(",\n")
    )
}

fn build_mysql_create_table(table: &str, cols: &[PgCol], pk: &[String]) -> String {
    let mut lines = vec![];
    for col in cols {
        let mysql_type = pg_type_to_mysql(col);
        let null_part = if col.is_nullable { "" } else { " NOT NULL" };
        lines.push(format!("  `{}` {}{}", col.name, mysql_type, null_part));
    }
    if !pk.is_empty() {
        let pk_cols = pk
            .iter()
            .map(|c| format!("`{}`", c))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("  PRIMARY KEY ({pk_cols})"));
    }
    format!(
        "CREATE TABLE IF NOT EXISTS `{}` (\n{}\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        table,
        lines.join(",\n")
    )
}

fn build_pg_index(table: &str, idx: &MysqlIdx) -> String {
    let unique = if idx.is_unique { "UNIQUE " } else { "" };
    let cols = idx
        .columns
        .iter()
        .map(|c| format!("\"{}\"", c))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CREATE {}INDEX IF NOT EXISTS \"{}\" ON \"{}\" ({})",
        unique, idx.name, table, cols
    )
}

fn build_pg_fk(table: &str, fk: &MysqlFk) -> String {
    format!(
        "ALTER TABLE \"{}\" ADD CONSTRAINT \"{}\" \
         FOREIGN KEY (\"{}\") REFERENCES \"{}\" (\"{}\")",
        table, fk.name, fk.column, fk.ref_table, fk.ref_column
    )
}

// ─── data transfer: MySQL → PostgreSQL ───────────────────────────────────────

async fn transfer_mysql_to_pg(
    src: &MySqlPool,
    dst: &PgPool,
    table: &str,
    cols: &[MysqlCol],
    pb: &ProgressBar,
) -> Result<u64> {
    // Clear target table (CREATE TABLE IF NOT EXISTS may leave data from previous runs)
    sqlx::query(&format!(
        "TRUNCATE TABLE \"{}\" RESTART IDENTITY CASCADE",
        table
    ))
    .execute(dst)
    .await
    .ok();

    let col_list = cols
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");

    let select_sql = format!("SELECT * FROM `{}`", table);
    let mut stream = sqlx::query(&select_sql).fetch(src);
    let mut batch: Vec<Vec<CellValue>> = Vec::with_capacity(CHUNK_SIZE);
    let mut total = 0u64;

    while let Some(row) = stream.try_next().await? {
        let cells: Vec<CellValue> = cols
            .iter()
            .enumerate()
            .map(|(i, col)| extract_mysql_cell(&row, i, col).unwrap_or(CellValue::Null))
            .collect();
        batch.push(cells);

        if batch.len() >= CHUNK_SIZE {
            insert_pg_batch(dst, table, &col_list, &batch).await?;
            total += batch.len() as u64;
            batch.clear();
            pb.set_message(format!("{}... {}", table, fmt_num(total)));
        }
    }

    if !batch.is_empty() {
        insert_pg_batch(dst, table, &col_list, &batch).await?;
        total += batch.len() as u64;
    }

    Ok(total)
}

async fn insert_pg_batch(
    dst: &PgPool,
    table: &str,
    col_list: &str,
    batch: &[Vec<CellValue>],
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let values: Vec<String> = batch
        .iter()
        .map(|row| {
            format!(
                "({})",
                row.iter()
                    .map(cell_to_pg_literal)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES {}",
        table,
        col_list,
        values.join(", ")
    );
    sqlx::query(&sql)
        .execute(dst)
        .await
        .map_err(|e| anyhow::anyhow!("INSERT into \"{table}\" failed: {e}"))?;
    Ok(())
}

// ─── data transfer: PostgreSQL → MySQL ───────────────────────────────────────

async fn transfer_pg_to_mysql(
    src: &PgPool,
    dst: &MySqlPool,
    table: &str,
    cols: &[PgCol],
    pb: &ProgressBar,
) -> Result<u64> {
    sqlx::query("SET FOREIGN_KEY_CHECKS=0")
        .execute(dst)
        .await
        .ok();
    sqlx::query(&format!("TRUNCATE TABLE `{}`", table))
        .execute(dst)
        .await
        .ok();
    sqlx::query("SET FOREIGN_KEY_CHECKS=1")
        .execute(dst)
        .await
        .ok();

    let col_list = cols
        .iter()
        .map(|c| format!("`{}`", c.name))
        .collect::<Vec<_>>()
        .join(", ");

    let select_sql = format!("SELECT * FROM \"{}\"", table);
    let mut stream = sqlx::query(&select_sql).fetch(src);
    let mut batch: Vec<Vec<CellValue>> = Vec::with_capacity(CHUNK_SIZE);
    let mut total = 0u64;

    while let Some(row) = stream.try_next().await? {
        let cells: Vec<CellValue> = cols
            .iter()
            .enumerate()
            .map(|(i, col)| extract_pg_cell(&row, i, col).unwrap_or(CellValue::Null))
            .collect();
        batch.push(cells);

        if batch.len() >= CHUNK_SIZE {
            insert_mysql_batch(dst, table, &col_list, &batch).await?;
            total += batch.len() as u64;
            batch.clear();
            pb.set_message(format!("{}... {}", table, fmt_num(total)));
        }
    }

    if !batch.is_empty() {
        insert_mysql_batch(dst, table, &col_list, &batch).await?;
        total += batch.len() as u64;
    }

    Ok(total)
}

async fn insert_mysql_batch(
    dst: &MySqlPool,
    table: &str,
    col_list: &str,
    batch: &[Vec<CellValue>],
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let values: Vec<String> = batch
        .iter()
        .map(|row| {
            format!(
                "({})",
                row.iter()
                    .map(cell_to_mysql_literal)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    let sql = format!(
        "INSERT INTO `{}` ({}) VALUES {}",
        table,
        col_list,
        values.join(", ")
    );
    sqlx::query(&sql)
        .execute(dst)
        .await
        .map_err(|e| anyhow::anyhow!("INSERT into `{table}` failed: {e}"))?;
    Ok(())
}

// ─── value extraction from MySQL rows ─────────────────────────────────────────

enum CellValue {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Decimal(Decimal),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(NaiveDateTime),
    Json(serde_json::Value),
}

fn extract_mysql_cell(
    row: &sqlx::mysql::MySqlRow,
    idx: usize,
    col: &MysqlCol,
) -> Result<CellValue> {
    let ct = col.column_type.as_str();

    // Boolean special case
    if ct == "tinyint(1)" {
        let v: Option<i8> = row.try_get(idx)?;
        return Ok(v
            .map(|n| CellValue::Bool(n != 0))
            .unwrap_or(CellValue::Null));
    }

    let without_paren = ct.find('(').map(|i| &ct[..i]).unwrap_or(ct);
    let base = without_paren
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let unsigned = ct.contains("unsigned");

    Ok(match base.as_str() {
        "tinyint" | "smallint" if unsigned => row
            .try_get::<Option<u16>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "tinyint" | "smallint" => row
            .try_get::<Option<i16>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "mediumint" | "int" | "integer" if unsigned => row
            .try_get::<Option<u32>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "mediumint" | "int" | "integer" => row
            .try_get::<Option<i32>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "bigint" if unsigned => row
            .try_get::<Option<u64>, _>(idx)?
            .map(CellValue::UInt)
            .unwrap_or(CellValue::Null),
        "bigint" => row
            .try_get::<Option<i64>, _>(idx)?
            .map(CellValue::Int)
            .unwrap_or(CellValue::Null),
        "float" => row
            .try_get::<Option<f32>, _>(idx)?
            .map(|v| CellValue::Float(v as f64))
            .unwrap_or(CellValue::Null),
        "double" | "real" => row
            .try_get::<Option<f64>, _>(idx)?
            .map(CellValue::Float)
            .unwrap_or(CellValue::Null),
        "decimal" | "numeric" => row
            .try_get::<Option<Decimal>, _>(idx)?
            .map(CellValue::Decimal)
            .unwrap_or(CellValue::Null),
        "year" => row
            .try_get::<Option<u16>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "varchar" | "char" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => {
            row.try_get::<Option<String>, _>(idx)?
                .map(CellValue::Text)
                .unwrap_or(CellValue::Null)
        }
        "json" => row
            .try_get::<Option<serde_json::Value>, _>(idx)?
            .map(CellValue::Json)
            .unwrap_or(CellValue::Null),
        "tinyblob" | "blob" | "mediumblob" | "longblob" | "binary" | "varbinary" | "bit" => row
            .try_get::<Option<Vec<u8>>, _>(idx)?
            .map(CellValue::Bytes)
            .unwrap_or(CellValue::Null),
        "date" => row
            .try_get::<Option<NaiveDate>, _>(idx)?
            .map(CellValue::Date)
            .unwrap_or(CellValue::Null),
        "time" => {
            // MySQL TIME can be > 24h or negative — fall back to Null if NaiveTime fails
            row.try_get::<Option<NaiveTime>, _>(idx)
                .ok()
                .flatten()
                .map(CellValue::Time)
                .unwrap_or(CellValue::Null)
        }
        "datetime" | "timestamp" => row
            .try_get::<Option<NaiveDateTime>, _>(idx)?
            .map(CellValue::DateTime)
            .unwrap_or(CellValue::Null),
        _ => {
            // Unknown type: try string
            row.try_get::<Option<String>, _>(idx)
                .unwrap_or(None)
                .map(CellValue::Text)
                .unwrap_or(CellValue::Null)
        }
    })
}

fn extract_pg_cell(row: &sqlx::postgres::PgRow, idx: usize, col: &PgCol) -> Result<CellValue> {
    Ok(match col.data_type.as_str() {
        "boolean" => row
            .try_get::<Option<bool>, _>(idx)?
            .map(CellValue::Bool)
            .unwrap_or(CellValue::Null),
        "smallint" => row
            .try_get::<Option<i16>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "integer" | "int" => row
            .try_get::<Option<i32>, _>(idx)?
            .map(|v| CellValue::Int(v as i64))
            .unwrap_or(CellValue::Null),
        "bigint" => row
            .try_get::<Option<i64>, _>(idx)?
            .map(CellValue::Int)
            .unwrap_or(CellValue::Null),
        "real" => row
            .try_get::<Option<f32>, _>(idx)?
            .map(|v| CellValue::Float(v as f64))
            .unwrap_or(CellValue::Null),
        "double precision" => row
            .try_get::<Option<f64>, _>(idx)?
            .map(CellValue::Float)
            .unwrap_or(CellValue::Null),
        "numeric" | "decimal" => row
            .try_get::<Option<Decimal>, _>(idx)?
            .map(CellValue::Decimal)
            .unwrap_or(CellValue::Null),
        "character" | "character varying" | "text" => row
            .try_get::<Option<String>, _>(idx)?
            .map(CellValue::Text)
            .unwrap_or(CellValue::Null),
        "bytea" => row
            .try_get::<Option<Vec<u8>>, _>(idx)?
            .map(CellValue::Bytes)
            .unwrap_or(CellValue::Null),
        "date" => row
            .try_get::<Option<NaiveDate>, _>(idx)?
            .map(CellValue::Date)
            .unwrap_or(CellValue::Null),
        "time without time zone" | "time with time zone" => row
            .try_get::<Option<NaiveTime>, _>(idx)?
            .map(CellValue::Time)
            .unwrap_or(CellValue::Null),
        "timestamp without time zone" => row
            .try_get::<Option<NaiveDateTime>, _>(idx)?
            .map(CellValue::DateTime)
            .unwrap_or(CellValue::Null),
        "timestamp with time zone" => {
            // Read as UTC, convert to NaiveDateTime (TZ info dropped for MySQL DATETIME)
            row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)?
                .map(|dt| CellValue::DateTime(dt.naive_utc()))
                .unwrap_or(CellValue::Null)
        }
        "json" | "jsonb" => row
            .try_get::<Option<serde_json::Value>, _>(idx)?
            .map(CellValue::Json)
            .unwrap_or(CellValue::Null),
        "uuid" => {
            // Try decoding as String (sqlx can coerce uuid to text)
            row.try_get::<Option<String>, _>(idx)
                .unwrap_or(None)
                .map(CellValue::Text)
                .unwrap_or(CellValue::Null)
        }
        "bit" | "bit varying" => row
            .try_get::<Option<Vec<u8>>, _>(idx)?
            .map(CellValue::Bytes)
            .unwrap_or(CellValue::Null),
        _ => {
            // Fallback: try string
            row.try_get::<Option<String>, _>(idx)
                .unwrap_or(None)
                .map(CellValue::Text)
                .unwrap_or(CellValue::Null)
        }
    })
}

// ─── cell → SQL literal ───────────────────────────────────────────────────────

fn cell_to_pg_literal(val: &CellValue) -> String {
    match val {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        CellValue::Int(n) => n.to_string(),
        CellValue::UInt(n) => n.to_string(),
        CellValue::Float(f) => {
            if f.is_nan() || f.is_infinite() {
                "NULL".into()
            } else {
                format!("{f}")
            }
        }
        CellValue::Decimal(d) => d.to_string(),
        CellValue::Text(s) => format!("'{}'", s.replace('\'', "''")),
        CellValue::Bytes(b) => format!("'\\x{}'", hex_encode(b)),
        CellValue::Date(d) => format!("'{}'", d.format("%Y-%m-%d")),
        CellValue::Time(t) => format!("'{}'", t.format("%H:%M:%S%.f")),
        CellValue::DateTime(dt) => format!("'{}'", dt.format("%Y-%m-%d %H:%M:%S%.f")),
        CellValue::Json(j) => format!("'{}'", j.to_string().replace('\'', "''")),
    }
}

fn cell_to_mysql_literal(val: &CellValue) -> String {
    match val {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(b) => if *b { "1" } else { "0" }.into(),
        CellValue::Int(n) => n.to_string(),
        CellValue::UInt(n) => n.to_string(),
        CellValue::Float(f) => format!("{f}"),
        CellValue::Decimal(d) => d.to_string(),
        CellValue::Text(s) => format!("'{}'", mysql_escape_str(s)),
        CellValue::Bytes(b) => format!("X'{}'", hex_encode(b)),
        CellValue::Date(d) => format!("'{}'", d.format("%Y-%m-%d")),
        CellValue::Time(t) => format!("'{}'", t.format("%H:%M:%S%.f")),
        CellValue::DateTime(dt) => format!("'{}'", dt.format("%Y-%m-%d %H:%M:%S%.f")),
        CellValue::Json(j) => format!("'{}'", mysql_escape_str(&j.to_string())),
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn get_container_ip(container_id: &str) -> Result<String> {
    let out = std::process::Command::new("docker")
        .args(["inspect", container_id])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("docker inspect failed for container {container_id}");
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    json[0]["NetworkSettings"]["Networks"]
        .as_object()
        .and_then(|nets| nets.values().next())
        .and_then(|net| net["IPAddress"].as_str())
        .filter(|ip| !ip.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Could not find IP for container {container_id}"))
}

async fn connect_mysql(config: &DbConfig, ip: &str) -> Result<MySqlPool> {
    let opts = MySqlConnectOptions::new()
        .host(ip)
        .port(3306)
        .username(&config.user)
        .password(&config.password)
        .database(&config.db_name)
        .ssl_mode(MySqlSslMode::Disabled);
    MySqlPool::connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to MySQL at {ip}: {e}"))
}

async fn connect_pg(config: &DbConfig, ip: &str) -> Result<PgPool> {
    let opts = PgConnectOptions::new()
        .host(ip)
        .port(5432)
        .username(&config.user)
        .password(&config.password)
        .database(&config.db_name)
        .ssl_mode(PgSslMode::Disable);
    PgPool::connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to PostgreSQL at {ip}: {e}"))
}

fn fmt_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mysql_escape_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\0', "\\0")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    // ── helpers ───────────────────────────────────────────────────────────────

    #[test]
    fn fmt_num_small() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(999), "999");
    }

    #[test]
    fn fmt_num_thousands() {
        assert_eq!(fmt_num(1_000), "1.0K");
        assert_eq!(fmt_num(1_500), "1.5K");
        assert_eq!(fmt_num(999_999), "1000.0K");
    }

    #[test]
    fn fmt_num_millions() {
        assert_eq!(fmt_num(1_000_000), "1.0M");
        assert_eq!(fmt_num(2_500_000), "2.5M");
    }

    #[test]
    fn hex_encode_basic() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_encode(&[0x00, 0xff]), "00ff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn mysql_escape_str_special_chars() {
        assert_eq!(mysql_escape_str("it's"), "it\\'s");
        assert_eq!(mysql_escape_str("a\\b"), "a\\\\b");
        assert_eq!(mysql_escape_str("line\nnext"), "line\\nnext");
        assert_eq!(mysql_escape_str("cr\rend"), "cr\\rend");
        assert_eq!(mysql_escape_str("nul\0byte"), "nul\\0byte");
        assert_eq!(mysql_escape_str("plain"), "plain");
    }

    // ── mysql_type_to_pg ──────────────────────────────────────────────────────

    fn mysql_col(column_type: &str) -> MysqlCol {
        MysqlCol {
            name: "col".into(),
            column_type: column_type.into(),
            is_nullable: true,
            is_auto_increment: false,
        }
    }

    fn mysql_col_ai(column_type: &str) -> MysqlCol {
        MysqlCol {
            name: "id".into(),
            column_type: column_type.into(),
            is_nullable: false,
            is_auto_increment: true,
        }
    }

    #[test]
    fn mysql_to_pg_tinyint1_is_boolean() {
        assert_eq!(mysql_type_to_pg(&mysql_col("tinyint(1)")), "BOOLEAN");
    }

    #[test]
    fn mysql_to_pg_int_auto_increment() {
        assert_eq!(mysql_type_to_pg(&mysql_col_ai("int(11)")), "SERIAL");
        assert_eq!(mysql_type_to_pg(&mysql_col_ai("bigint(20)")), "BIGSERIAL");
    }

    #[test]
    fn mysql_to_pg_integers() {
        assert_eq!(mysql_type_to_pg(&mysql_col("tinyint")), "SMALLINT");
        assert_eq!(mysql_type_to_pg(&mysql_col("smallint")), "SMALLINT");
        assert_eq!(mysql_type_to_pg(&mysql_col("mediumint")), "INTEGER");
        assert_eq!(mysql_type_to_pg(&mysql_col("int")), "INTEGER");
        assert_eq!(mysql_type_to_pg(&mysql_col("bigint")), "BIGINT");
    }

    #[test]
    fn mysql_to_pg_unsigned_integers() {
        assert_eq!(mysql_type_to_pg(&mysql_col("int unsigned")), "BIGINT");
        assert_eq!(
            mysql_type_to_pg(&mysql_col("bigint unsigned")),
            "NUMERIC(20,0)"
        );
    }

    #[test]
    fn mysql_to_pg_floats() {
        assert_eq!(mysql_type_to_pg(&mysql_col("float")), "REAL");
        assert_eq!(mysql_type_to_pg(&mysql_col("double")), "DOUBLE PRECISION");
    }

    #[test]
    fn mysql_to_pg_decimal_preserves_precision() {
        assert_eq!(
            mysql_type_to_pg(&mysql_col("decimal(10,2)")),
            "NUMERIC(10,2)"
        );
        assert_eq!(mysql_type_to_pg(&mysql_col("decimal")), "NUMERIC");
    }

    #[test]
    fn mysql_to_pg_varchar_preserves_length() {
        assert_eq!(mysql_type_to_pg(&mysql_col("varchar(255)")), "VARCHAR(255)");
        assert_eq!(mysql_type_to_pg(&mysql_col("char(10)")), "CHAR(10)");
    }

    #[test]
    fn mysql_to_pg_text_types() {
        for t in &["tinytext", "text", "mediumtext", "longtext"] {
            assert_eq!(mysql_type_to_pg(&mysql_col(t)), "TEXT", "failed for {t}");
        }
    }

    #[test]
    fn mysql_to_pg_blob_types() {
        for t in &[
            "blob",
            "tinyblob",
            "mediumblob",
            "longblob",
            "binary",
            "varbinary",
        ] {
            assert_eq!(mysql_type_to_pg(&mysql_col(t)), "BYTEA", "failed for {t}");
        }
    }

    #[test]
    fn mysql_to_pg_datetime_types() {
        assert_eq!(mysql_type_to_pg(&mysql_col("date")), "DATE");
        assert_eq!(mysql_type_to_pg(&mysql_col("time")), "TIME");
        assert_eq!(mysql_type_to_pg(&mysql_col("datetime")), "TIMESTAMP");
        assert_eq!(mysql_type_to_pg(&mysql_col("timestamp")), "TIMESTAMPTZ");
        assert_eq!(mysql_type_to_pg(&mysql_col("year")), "SMALLINT");
    }

    #[test]
    fn mysql_to_pg_json() {
        assert_eq!(mysql_type_to_pg(&mysql_col("json")), "JSONB");
    }

    #[test]
    fn mysql_to_pg_unknown_falls_back_to_text() {
        assert_eq!(mysql_type_to_pg(&mysql_col("geometry")), "TEXT");
    }

    // ── pg_type_to_mysql ──────────────────────────────────────────────────────

    fn pg_col(data_type: &str) -> PgCol {
        PgCol {
            name: "col".into(),
            data_type: data_type.into(),
            char_max_len: None,
            numeric_precision: None,
            numeric_scale: None,
            is_nullable: true,
            is_serial: false,
        }
    }

    fn pg_col_serial(data_type: &str) -> PgCol {
        PgCol {
            is_serial: true,
            ..pg_col(data_type)
        }
    }

    fn pg_col_varchar(len: i32) -> PgCol {
        PgCol {
            data_type: "character varying".into(),
            char_max_len: Some(len),
            ..pg_col("character varying")
        }
    }

    fn pg_col_numeric(p: i32, s: i32) -> PgCol {
        PgCol {
            data_type: "numeric".into(),
            numeric_precision: Some(p),
            numeric_scale: Some(s),
            ..pg_col("numeric")
        }
    }

    #[test]
    fn pg_to_mysql_serial_types() {
        assert_eq!(
            pg_type_to_mysql(&pg_col_serial("integer")),
            "INT AUTO_INCREMENT"
        );
        assert_eq!(
            pg_type_to_mysql(&pg_col_serial("bigint")),
            "BIGINT AUTO_INCREMENT"
        );
    }

    #[test]
    fn pg_to_mysql_integers() {
        assert_eq!(pg_type_to_mysql(&pg_col("boolean")), "TINYINT(1)");
        assert_eq!(pg_type_to_mysql(&pg_col("smallint")), "SMALLINT");
        assert_eq!(pg_type_to_mysql(&pg_col("integer")), "INT");
        assert_eq!(pg_type_to_mysql(&pg_col("bigint")), "BIGINT");
    }

    #[test]
    fn pg_to_mysql_floats() {
        assert_eq!(pg_type_to_mysql(&pg_col("real")), "FLOAT");
        assert_eq!(pg_type_to_mysql(&pg_col("double precision")), "DOUBLE");
    }

    #[test]
    fn pg_to_mysql_numeric_preserves_precision() {
        assert_eq!(pg_type_to_mysql(&pg_col_numeric(10, 2)), "DECIMAL(10,2)");
        assert_eq!(pg_type_to_mysql(&pg_col("numeric")), "DECIMAL");
    }

    #[test]
    fn pg_to_mysql_varchar_preserves_length() {
        assert_eq!(pg_type_to_mysql(&pg_col_varchar(100)), "VARCHAR(100)");
        assert_eq!(pg_type_to_mysql(&pg_col("text")), "LONGTEXT");
    }

    #[test]
    fn pg_to_mysql_temporal_types() {
        assert_eq!(pg_type_to_mysql(&pg_col("date")), "DATE");
        assert_eq!(pg_type_to_mysql(&pg_col("time without time zone")), "TIME");
        assert_eq!(
            pg_type_to_mysql(&pg_col("timestamp without time zone")),
            "DATETIME"
        );
        assert_eq!(
            pg_type_to_mysql(&pg_col("timestamp with time zone")),
            "DATETIME"
        );
    }

    #[test]
    fn pg_to_mysql_misc_types() {
        assert_eq!(pg_type_to_mysql(&pg_col("jsonb")), "JSON");
        assert_eq!(pg_type_to_mysql(&pg_col("json")), "JSON");
        assert_eq!(pg_type_to_mysql(&pg_col("uuid")), "CHAR(36)");
        assert_eq!(pg_type_to_mysql(&pg_col("bytea")), "LONGBLOB");
    }

    // ── DDL builders ──────────────────────────────────────────────────────────

    #[test]
    fn build_pg_create_table_no_pk() {
        let cols = vec![
            MysqlCol {
                name: "id".into(),
                column_type: "int".into(),
                is_nullable: false,
                is_auto_increment: true,
            },
            MysqlCol {
                name: "name".into(),
                column_type: "varchar(100)".into(),
                is_nullable: true,
                is_auto_increment: false,
            },
        ];
        let ddl = build_pg_create_table("users", &cols, &[]);
        assert!(ddl.starts_with("CREATE TABLE IF NOT EXISTS \"users\""));
        assert!(ddl.contains("\"id\" SERIAL NOT NULL"));
        assert!(ddl.contains("\"name\" VARCHAR(100)"));
        assert!(!ddl.contains("PRIMARY KEY"));
    }

    #[test]
    fn build_pg_create_table_with_pk() {
        let cols = vec![MysqlCol {
            name: "id".into(),
            column_type: "int".into(),
            is_nullable: false,
            is_auto_increment: false,
        }];
        let pk = vec!["id".to_string()];
        let ddl = build_pg_create_table("items", &cols, &pk);
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
    }

    #[test]
    fn build_mysql_create_table_with_pk() {
        let cols = vec![PgCol {
            name: "id".into(),
            data_type: "integer".into(),
            char_max_len: None,
            numeric_precision: None,
            numeric_scale: None,
            is_nullable: false,
            is_serial: true,
        }];
        let pk = vec!["id".to_string()];
        let ddl = build_mysql_create_table("orders", &cols, &pk);
        assert!(ddl.starts_with("CREATE TABLE IF NOT EXISTS `orders`"));
        assert!(ddl.contains("`id` INT AUTO_INCREMENT NOT NULL"));
        assert!(ddl.contains("PRIMARY KEY (`id`)"));
        assert!(ddl.contains("ENGINE=InnoDB"));
    }

    #[test]
    fn build_pg_index_unique() {
        let idx = MysqlIdx {
            name: "uq_email".into(),
            columns: vec!["email".into()],
            is_unique: true,
        };
        let ddl = build_pg_index("users", &idx);
        assert_eq!(
            ddl,
            "CREATE UNIQUE INDEX IF NOT EXISTS \"uq_email\" ON \"users\" (\"email\")"
        );
    }

    #[test]
    fn build_pg_index_non_unique_multi_col() {
        let idx = MysqlIdx {
            name: "idx_name_age".into(),
            columns: vec!["last_name".into(), "age".into()],
            is_unique: false,
        };
        let ddl = build_pg_index("users", &idx);
        assert_eq!(
            ddl,
            "CREATE INDEX IF NOT EXISTS \"idx_name_age\" ON \"users\" (\"last_name\", \"age\")"
        );
    }

    #[test]
    fn build_pg_fk_basic() {
        let fk = MysqlFk {
            name: "fk_order_user".into(),
            column: "user_id".into(),
            ref_table: "users".into(),
            ref_column: "id".into(),
        };
        let ddl = build_pg_fk("orders", &fk);
        assert_eq!(
            ddl,
            "ALTER TABLE \"orders\" ADD CONSTRAINT \"fk_order_user\" \
             FOREIGN KEY (\"user_id\") REFERENCES \"users\" (\"id\")"
        );
    }

    // ── cell_to_pg_literal ────────────────────────────────────────────────────

    #[test]
    fn pg_literal_null() {
        assert_eq!(cell_to_pg_literal(&CellValue::Null), "NULL");
    }

    #[test]
    fn pg_literal_bool() {
        assert_eq!(cell_to_pg_literal(&CellValue::Bool(true)), "TRUE");
        assert_eq!(cell_to_pg_literal(&CellValue::Bool(false)), "FALSE");
    }

    #[test]
    fn pg_literal_integers() {
        assert_eq!(cell_to_pg_literal(&CellValue::Int(42)), "42");
        assert_eq!(cell_to_pg_literal(&CellValue::Int(-1)), "-1");
        assert_eq!(
            cell_to_pg_literal(&CellValue::UInt(u64::MAX)),
            u64::MAX.to_string()
        );
    }

    #[test]
    fn pg_literal_text_escapes_single_quotes() {
        assert_eq!(
            cell_to_pg_literal(&CellValue::Text("it's".into())),
            "'it''s'"
        );
        assert_eq!(
            cell_to_pg_literal(&CellValue::Text("plain".into())),
            "'plain'"
        );
    }

    #[test]
    fn pg_literal_bytes_hex() {
        assert_eq!(
            cell_to_pg_literal(&CellValue::Bytes(vec![0xca, 0xfe])),
            "'\\xcafe'"
        );
    }

    #[test]
    fn pg_literal_date() {
        let d = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        assert_eq!(cell_to_pg_literal(&CellValue::Date(d)), "'2024-06-15'");
    }

    #[test]
    fn pg_literal_float_nan_becomes_null() {
        assert_eq!(cell_to_pg_literal(&CellValue::Float(f64::NAN)), "NULL");
        assert_eq!(cell_to_pg_literal(&CellValue::Float(f64::INFINITY)), "NULL");
    }

    // ── cell_to_mysql_literal ─────────────────────────────────────────────────

    #[test]
    fn mysql_literal_null() {
        assert_eq!(cell_to_mysql_literal(&CellValue::Null), "NULL");
    }

    #[test]
    fn mysql_literal_bool_as_int() {
        assert_eq!(cell_to_mysql_literal(&CellValue::Bool(true)), "1");
        assert_eq!(cell_to_mysql_literal(&CellValue::Bool(false)), "0");
    }

    #[test]
    fn mysql_literal_text_escapes() {
        assert_eq!(
            cell_to_mysql_literal(&CellValue::Text("it's".into())),
            "'it\\'s'"
        );
        assert_eq!(
            cell_to_mysql_literal(&CellValue::Text("a\\b".into())),
            "'a\\\\b'"
        );
    }

    #[test]
    fn mysql_literal_bytes_hex() {
        assert_eq!(
            cell_to_mysql_literal(&CellValue::Bytes(vec![0xde, 0xad])),
            "X'dead'"
        );
    }

    #[test]
    fn mysql_literal_date() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        assert_eq!(cell_to_mysql_literal(&CellValue::Date(d)), "'2024-01-01'");
    }
}
