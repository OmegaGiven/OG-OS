mod config;
mod html;
mod store;

use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use config::Config;
use store::Alias;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    aliases: Arc<Mutex<Vec<Alias>>>,
}

#[derive(Deserialize)]
struct AddForm {
    key: String,
    url: String,
}

#[derive(Deserialize)]
struct DeleteForm {
    key: String,
}

async fn home(State(state): State<AppState>) -> Html<String> {
    let aliases = state.aliases.lock().unwrap().clone();
    Html(html::home_page(&aliases, &state.config, None, ""))
}

async fn add(State(state): State<AppState>, Form(form): Form<AddForm>) -> Redirect {
    let key = store::normalize_key(&form.key);
    let url = store::normalize_url(&form.url);
    if !key.is_empty() && !url.is_empty() {
        let mut aliases = state.aliases.lock().unwrap();
        aliases.retain(|a| a.key != key);
        aliases.push(Alias { key, url });
        aliases.sort_by(|a, b| a.key.cmp(&b.key));
        store::save(&aliases);
    }
    Redirect::to("/")
}

async fn delete(State(state): State<AppState>, Form(form): Form<DeleteForm>) -> Redirect {
    let key = store::normalize_key(&form.key);
    let mut aliases = state.aliases.lock().unwrap();
    aliases.retain(|a| a.key != key);
    store::save(&aliases);
    Redirect::to("/")
}

async fn lookup(State(state): State<AppState>, Path(key): Path<String>) -> Response {
    let key = store::normalize_key(&key);
    let aliases = state.aliases.lock().unwrap().clone();

    match aliases.iter().find(|a| a.key == key) {
        Some(alias) => Redirect::to(&alias.url).into_response(),
        None => {
            let notice = format!("No shortcut named \"{key}\" yet — add it below.");
            Html(html::home_page(&aliases, &state.config, Some(&notice), &key)).into_response()
        }
    }
}

#[tokio::main]
async fn main() {
    let state = AppState {
        config: Arc::new(Config::load()),
        aliases: Arc::new(Mutex::new(store::load())),
    };

    let app = Router::new()
        .route("/", get(home))
        .route("/add", post(add))
        .route("/delete", post(delete))
        .route("/:key", get(lookup))
        .with_state(state);

    let port: u16 = std::env::var("OG_LINKS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(80);

    let addr = format!("127.0.0.1:{port}");
    match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => {
            println!("og-links listening on http://{addr}");
            axum::serve(listener, app).await.unwrap();
        }
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            if port == 80 {
                let exe = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "path/to/og-links".to_string());
                eprintln!("port 80 needs a privilege grant, e.g.:");
                eprintln!("  sudo setcap 'cap_net_bind_service=+ep' {exe}");
            }
            std::process::exit(1);
        }
    }
}
