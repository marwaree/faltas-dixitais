use axum::{Form, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use nanoid::nanoid;
use serde::Deserialize;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::time::{Duration, Instant};
use tower_http::services::ServeFile;

use std::sync::{Arc, Mutex};

struct ActiveCode {
    code: String,
    expiry: Instant,
}

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    active_code: Arc<Mutex<Option<ActiveCode>>>,
}

#[derive(Deserialize)]
struct SubmitPayload {
    name: String,
    code: String,
}

#[tokio::main]
async fn main() {
    // Connect to SQLite file database (creates app.db if it doesn't exist)
    let db = SqlitePoolOptions::new()
        .connect("sqlite://app.db?mode=rwc")
        .await
        .expect("Failed to connect to SQLite");

    // Initialize the submissions table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS submissions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create table");

    let state = AppState {
        db,
        active_code: Arc::new(Mutex::new(None)),
    };

    let app = Router::new()
        .route("/submit", post(submit_code))
        .route("/get-active-code", post(get_active_code))
        .route("/clear-entries", post(clear_entries))
        .route_service("/profesor", ServeFile::new("templates/profesor.html"))
        .fallback_service(ServeFile::new("templates/index.html"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081").await.unwrap();

    println!("Server listening on http://0.0.0.0:8081");
    axum::serve(listener, app).await.unwrap();
}

async fn get_active_code(State(state): State<AppState>) -> String {
    let digits: [char; 10] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
    let now = Instant::now();

    // 1. Memory operation inside a scope (Lock released before await)
    let (code, remaining) = {
        let mut lock = state.active_code.lock().unwrap();
        if let Some(ref active) = *lock {
            if active.expiry > now {
                (
                    active.code.clone(),
                    active.expiry.saturating_duration_since(now).as_secs(),
                )
            } else {
                generate_new_code(&mut lock, now, &digits)
            }
        } else {
            generate_new_code(&mut lock, now, &digits)
        }
    };

    // 2. Fetch names sorted alphabetically (case-insensitive)
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM submissions ORDER BY name COLLATE NOCASE ASC")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

    let names = rows.into_iter().map(|r| r.0).collect::<Vec<_>>().join(", ");

    format!("{}:{}:{}", code, remaining, names)
}

fn generate_new_code(
    lock: &mut Option<ActiveCode>,
    now: Instant,
    digits: &[char; 10],
) -> (String, u64) {
    let new_code = nanoid!(6, digits);
    let ttl = 30;
    *lock = Some(ActiveCode {
        code: new_code.clone(),
        expiry: now + Duration::from_secs(ttl),
    });
    (new_code, ttl)
}

async fn submit_code(State(state): State<AppState>, Form(payload): Form<SubmitPayload>) -> String {
    let clean_name = payload.name.trim().to_string();
    let clean_code = payload.code.trim().to_string();

    // Check code validity and drop the lock BEFORE performing async DB operations
    let is_valid = {
        let lock = state.active_code.lock().unwrap();
        let now = Instant::now();
        if let Some(ref active) = *lock {
            active.expiry > now && active.code == clean_code
        } else {
            false
        }
    }; // MutexGuard is dropped here

    if is_valid {
        if !clean_name.is_empty() {
            let _ = sqlx::query("INSERT INTO submissions (name) VALUES (?)")
                .bind(clean_name)
                .execute(&state.db)
                .await;
        }
        "SUCCESS".to_string()
    } else {
        "INVALID_OR_EXPIRED".to_string()
    }
}

async fn clear_entries(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query("DELETE FROM submissions")
        .execute(&state.db)
        .await
    {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            eprintln!("Error clearing database: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
