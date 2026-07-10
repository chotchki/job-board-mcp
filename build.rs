//! Compile-time schema for sqlx's `query!` macros.
//!
//! Instead of committing an offline `.sqlx` query cache and keeping it in sync, this
//! migrates a throwaway `schema.db` in `OUT_DIR` from the current `migrations/` and
//! points `DATABASE_URL` at it via `cargo::rustc-env`. So `query!` always type-checks
//! against a freshly-migrated schema — nothing to regenerate, nothing to go stale, and
//! it's hermetic on every CI runner.

use std::env;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::main]
async fn main() -> Result<()> {
    // Re-run when the schema or this script changes — not on every source edit.
    println!("cargo::rerun-if-changed=migrations");
    println!("cargo::rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR").context("OUT_DIR not set — cargo is misbehaving")?;
    let db_path = Path::new(&out_dir).join("schema.db");

    // Rebuild the scratch schema from scratch every time, so an edited migration can't
    // leave a stale schema behind for `query!` to check against.
    let _ = std::fs::remove_file(&db_path);

    // sqlite:// URL. On Windows the path is `C:\...` — forward-slash it and ensure a
    // leading slash so we get the documented absolute form `sqlite:///C:/...`; on Unix
    // the path already starts with `/`, giving `sqlite:///...`.
    let forward = db_path.to_string_lossy().replace('\\', "/");
    let leading = if forward.starts_with('/') {
        forward.clone()
    } else {
        format!("/{forward}")
    };
    let url = format!("sqlite://{leading}");
    println!("cargo::rustc-env=DATABASE_URL={url}");

    // Convenience for editors / rust-analyzer's sqlx integration; regenerated here, and
    // gitignored because it holds a machine-absolute path.
    let manifest = env::var("CARGO_MANIFEST_DIR")?;
    std::fs::write(
        Path::new(&manifest).join(".env"),
        format!("DATABASE_URL={url}\n"),
    )
    .context("writing .env")?;

    // Connect by filename (no URL parsing) and migrate.
    let options = SqliteConnectOptions::from_str(&url)
        .with_context(|| format!("parsing sqlite url {url}"))?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .context("opening the scratch schema database")?;
    // Read the migrations at build-script RUNTIME rather than embedding them with
    // `migrate!`. The macro bakes the migration list into this script at ITS compile
    // time, so adding a migration file only re-RUNS the cached script (against a stale
    // embedded set) and `query!` type-checks against a schema missing the new table.
    // `Migrator::new(Path)` re-reads the directory every run, so a new migration lands
    // without a `touch build.rs` dance.
    let migrator = sqlx::migrate::Migrator::new(Path::new("./migrations"))
        .await
        .context("reading the migrations directory")?;
    migrator
        .run(&pool)
        .await
        .context("running migrations against the scratch schema")?;
    Ok(())
}
