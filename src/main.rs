#[forbid(unsafe_code)]
use anyhow::{Context, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    http::{
        header::{CONTENT_TYPE, LOCATION, SET_COOKIE},
        HeaderName, HeaderValue, Response, StatusCode,
    }, response::IntoResponse, routing::{get, post}, Form, Router
};
use axum_extra::extract::cookie::{Cookie, SameSite, CookieJar};
use base64::prelude::*;
use jsonwebtoken::{decode, DecodingKey, EncodingKey, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::UNIX_EPOCH;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    signal,
};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    // Validate required files and directories
    if let Err(_) = File::open("secret").await {
        println!("Initializing secret file");
        let mut f = File::create("secret")
            .await
            .expect("Could not create secret file");

        let key: [u8; 32] = rand::thread_rng().gen();
        let secret = BASE64_STANDARD.encode(key);

        f.write_all(secret.as_bytes())
            .await
            .expect("Could not init secret file");
    }

    if let Err(_) = File::open("config.json").await {
        println!("Initializing config.json");
        let mut f = File::create("config.json").await.expect("Cannot create config.json");

        let default_config = Config {
            secure_auth_cookie: true
        };

        f.write_all(serde_json::to_string_pretty(&default_config).expect("Could not serialize default config").as_bytes()).await.expect("Could not initialize config.json")
    }

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/styles.css", get(css_handler))
        .route(
            "/htmx.min.js",
            get(|| async { include_str!("./html/htmx.min.js") }),
        )
        .route("/app.js", get(|| async { include_str!("./html/app.js") }))
        .route("/spinner.svg", get(spinner_handler))
        .route("/login", get(login_page_handler))
        .route("/login", post(login_handler))
        .route("/login/create", get(create_login_page_handler))
        .route("/login/create", post(create_login_handler))
        .fallback(get(not_found_handler));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .context("Could not bind TCP listener")?;

    println!(" -> LISTENING ON: 0.0.0.0:3000");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Could not serve app")?;

    Ok(())
}

// Basic structs

#[derive(Serialize, Deserialize)]
struct Config {
    secure_auth_cookie: bool
}

// region: basic pages

async fn index_handler(jar: CookieJar) -> impl IntoResponse {
    if verify_auth(jar).await {
        return Response::builder()
            .status(StatusCode::OK)
            .body(String::from(include_str!("./html/index.html")))
            .unwrap()
    } else {
        return Response::builder() 
            .status(StatusCode::SEE_OTHER)
            .header(LOCATION, HeaderValue::from_static("/login"))
            .body(String::new())
            .unwrap()
    }
}

async fn css_handler() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/css")
        .body(String::from(include_str!("./html/styles.css")))
        .unwrap()
}

async fn not_found_handler() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(String::from(include_str!("./html/status_codes/404.html")))
        .unwrap()
}

// endregion: basic pages
// region: image routing

async fn spinner_handler() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("image/svg+xml"))
        .body(String::from(include_str!("./html/img/spinner.svg")))
        .unwrap()
}

// endregion: image routing
// region: login

async fn create_login_page_handler() -> impl IntoResponse {
    if let Ok(_) = File::open("login.json").await {
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(LOCATION, HeaderValue::from_static("/login"))
            .body(String::new())
            .unwrap();
    } else {
        return Response::builder()
            .status(StatusCode::OK)
            .body(String::from(include_str!("./html/create_login.html")))
            .unwrap();
    }
}

#[derive(Serialize, Deserialize)]
struct CreateLogin {
    username: String,
    password: String,
    confirm_password: String,
}

#[derive(Serialize, Deserialize)]
struct Login {
    username: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    un: String,
    exp: usize,
}

async fn create_login_handler(Form(data): Form<CreateLogin>) -> impl IntoResponse {
    if let Ok(_) = File::open("login.json").await {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(String::new())
            .unwrap();
    } else if !data.username.contains(" ")
        && !data.username.is_empty()
        && !data.password.contains(" ")
        && !data.password.is_empty()
    {
        if data.password == data.confirm_password {
            let password_hash = tokio::task::spawn_blocking(move || {
                let salt = SaltString::generate(&mut OsRng);
                let argon2 = Argon2::default();
                argon2
                    .hash_password(data.password.as_bytes(), &salt)
                    .unwrap()
                    .to_string()
            })
            .await
            .unwrap();

            let mut f = File::create("login.json")
                .await
                .expect("Could not create login.json");

            let new_login = Login {
                username: data.username,
                password: password_hash,
            };

            f.write_all(serde_json::to_string(&new_login).unwrap().as_bytes())
                .await
                .expect("Could not init login.json");

            return Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(SET_COOKIE, auth_cookie_builder(new_login.username).await)
                .header(
                    HeaderName::from_static("hx-redirect"),
                    HeaderValue::from_static("/"),
                )
                .body(String::new())
                .unwrap();
        } else {
            return Response::builder()
                .status(StatusCode::OK)
                .body(String::from("<p>Passwords do not match</p>"))
                .unwrap();
        }
    } else {
        return Response::builder()
            .status(StatusCode::OK)
            .body(String::from(
                "<p>Username and password cannot be empty or contain spaces</p>",
            ))
            .unwrap();
    }
}

async fn login_page_handler() -> impl IntoResponse {
    if let Ok(_) = File::open("login.json").await {
        return Response::builder()
            .status(StatusCode::OK)
            .body(String::from(include_str!("./html/login.html")))
            .unwrap();
    } else {
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(LOCATION, HeaderValue::from_static("/login/create"))
            .body(String::new())
            .unwrap();
    }
}

async fn login_handler(Form(data): Form<Login>) -> impl IntoResponse {
    if let Ok(f) = File::open("login.json").await {
        if !data.username.contains(" ")
            && !data.username.is_empty()
            && !data.password.contains(" ")
            && !data.password.is_empty()
        {
            let mut buf = String::new();
            let mut buf_reader = BufReader::new(f);

            buf_reader
                .read_to_string(&mut buf)
                .await
                .expect("Could not read login.json");

            let hashed_login: Login =
                serde_json::from_str(&buf).expect("Could not deserialize login.json");

            if data.username == hashed_login.username {
                if tokio::task::spawn_blocking(move || {
                    Argon2::default()
                        .verify_password(
                            data.password.as_bytes(),
                            &PasswordHash::new(&hashed_login.password)
                                .expect("Could not parse password hash"),
                        )
                        .is_ok()
                })
                .await
                .expect("Could not verify hash")
                {
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header(SET_COOKIE, auth_cookie_builder(data.username).await)
                        .header(
                            HeaderName::from_static("hx-redirect"),
                            HeaderValue::from_static("/"),
                        )
                        .body(String::new())
                        .unwrap();
                } else {
                    return Response::builder()
                        .status(StatusCode::OK)
                        .body(String::from("Invalid login"))
                        .unwrap();
                }
            } else {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(String::from("Invalid login"))
                    .unwrap();
            }
        } else {
            return Response::builder()
                .status(StatusCode::OK)
                .body(String::from(
                    "<p>Username and password cannot be empty or contain spaces</p>",
                ))
                .unwrap();
        }
    } else {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(String::new())
            .unwrap();
    }
}

async fn auth_cookie_builder(username: String) -> String {
    let mut secret = String::new();

    let secret_f = File::open("secret")
        .await
        .expect("Could not open secret file");
    let mut buf_reader = BufReader::new(secret_f);

    if let Err(_) = buf_reader.read_to_string(&mut secret).await {
        panic!("Cannot read secret file! Generating a auth token with an empty private key is unsecure!");
    };

    let claims = Claims {
        sub: Uuid::new_v4().to_string(),
        un: username,
        exp: (std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs()
            + std::time::Duration::from_secs(60 * 60 * 24 * 7).as_secs()) as usize,
    };

    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("Could not create auth token!");

    let mut config_str = String::new();

    let config_f = File::open("config.json").await.expect("Could not open config.json");
    let mut buf_reader = BufReader::new(config_f);
    buf_reader.read_to_string(&mut config_str).await.expect("Could not read config.json");

    let config: Config = serde_json::from_str(&config_str).expect("Could not deserialize config.json");

    let cookie = Cookie::build(("AuthToken", token))
        .path("/")
        .secure(config.secure_auth_cookie)
        .http_only(true)
        .same_site(SameSite::Strict);

    cookie.to_string()
}

async fn verify_auth(jar: CookieJar) -> bool {
    if let Some(auth_token) = jar.get("AuthToken") {
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        let mut secret = String::new();

        let secret_f = File::open("secret").await.expect("Could not open secret file");
        let mut buf_reader = BufReader::new(secret_f);
        buf_reader.read_to_string(&mut secret).await.expect("Could not read secret file");

        if let Ok(_) = decode::<Claims>(&auth_token.value(), &DecodingKey::from_secret(secret.as_bytes()), &validation) {
            return true;
        } else {
            return false;
        }
    } else {
        return false;
    }
}

// endregion: login

// Code borrowed from https://github.com/tokio-rs/axum/blob/806bc26e62afc2e0c83240a9e85c14c96bc2ceb3/examples/graceful-shutdown/src/main.rs
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
