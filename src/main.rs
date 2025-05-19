use actix_web::HttpRequest;
use actix_web::cookie::Cookie;
use actix_web::{App, HttpResponse, HttpServer, Responder, get, http::header, post, web};
use once_cell::sync::Lazy;
use rand::{Rng as _, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, to_string};
use sled::Db;
use std::time::{SystemTime, UNIX_EPOCH};

static NOT_FOUND: Lazy<String> =
    Lazy::new(move || std::fs::read_to_string("static/notfound.html").unwrap());
static ERROR: Lazy<String> =
    Lazy::new(move || std::fs::read_to_string("static/error.html").unwrap());
static CREATE: Lazy<String> =
    Lazy::new(move || std::fs::read_to_string("static/create.html").unwrap());
static SECRET: Lazy<String> =
    Lazy::new(move || std::fs::read_to_string("static/secret.html").unwrap());
static HOME: Lazy<String> =
    Lazy::new(move || std::fs::read_to_string("static/index.html").unwrap());

#[derive(Serialize, Deserialize)]
struct Secret {
    from: String,
    kind: SecretKind,
    count: Option<u64>,
    created_at: u64,
}

#[derive(Serialize, Deserialize)]
enum SecretKind {
    Link(String),
    Image(String),
    Text(String),
}

#[post("/api/secret")]
async fn create(body: String, db: web::Data<Db>) -> impl Responder {
    let wow = match serde_json::from_str::<Value>(&body) {
        Ok(r) => r,
        Err(_) => return HttpResponse::Ok().json(json!({"error": "invalid data"})),
    };
    let from = wow["from"].as_str().unwrap_or("anonymous");
    let link_id = rand::rng()
        .sample_iter(Alphanumeric)
        .take(10)
        .map(char::from)
        .collect::<String>();
    let kind = match wow["link"].as_str() {
        Some(r) => SecretKind::Link(r.to_string()),
        None => match wow["text"].as_str() {
            Some(r) => SecretKind::Text(r.to_string()),
            None => match wow["image"].as_str() {
                Some(r) => SecretKind::Image(r.to_string()),
                None => {
                    return HttpResponse::Ok()
                        .json(json!({"error": "Image, text or image was not included"}));
                }
            },
        },
    };
    let secret = Secret {
        from: from.to_string(),
        kind,
        count: wow["count"].as_u64(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };

    let _ = db.insert(format!("link:{}", link_id), &*to_string(&secret).unwrap());

    HttpResponse::Ok().json(json!({"link": link_id}))
}

#[get("/create/")]
async fn create_html() -> impl Responder {
    HttpResponse::Ok().content_type("text/html").body(&**CREATE)
}

#[get("/link/{link}")]
async fn get_link(path: web::Path<String>, db: web::Data<Db>, req: HttpRequest) -> impl Responder {
    let link = path.into_inner();
    let key = format!("link:{}", &link);

    // 🍪 Check or generate viewer ID
    let mut viewer_id = req
        .cookie("ln_viewer")
        .map(|c| c.value().to_string())
        .unwrap_or_else(|| {
            rand::rng()
                .sample_iter(Alphanumeric)
                .take(15)
                .map(char::from)
                .collect()
        });

    let viewed_key = format!("viewed:{}:{}", link, viewer_id);

    // ❌ Block if already viewed
    if db.contains_key(&viewed_key).unwrap_or(false) {
        return HttpResponse::Ok()
            .content_type("text/html")
            .body(&**NOT_FOUND);
    }

    let Some(raw) = db.get(&key).ok().flatten() else {
        return HttpResponse::Ok()
            .content_type("text/html")
            .body(&**NOT_FOUND);
    };

    let Ok(mut data) = serde_json::from_slice::<Secret>(&raw) else {
        return HttpResponse::Ok().content_type("text/html").body(&**ERROR);
    };

    // Check expiration
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now - data.created_at > 3600 {
        let _ = db.remove(&key);
        return HttpResponse::Ok()
            .content_type("text/html")
            .body(&**NOT_FOUND);
    }

    // Check count BEFORE returning anything
    match data.count {
        Some(0) => {
            let _ = db.remove(&key);
            return HttpResponse::Ok()
                .content_type("text/html")
                .body(&**NOT_FOUND);
        }
        Some(1) => {
            // Final view — delete BEFORE responding
            let _ = db.remove(&key);
        }
        Some(n) if n > 1 => {
            data.count = Some(n - 1);
            let _ = db.insert(&key, &*to_string(&data).unwrap());
        }
        None => {
            // One-time secret — delete instantly
            let _ = db.remove(&key);
        }
        _ => {}
    }

    let _ = db.insert(&viewed_key, "1");
    let mut response = match data.kind {
        SecretKind::Link(link) => HttpResponse::Found()
            .insert_header((header::LOCATION, link))
            .finish(),
        SecretKind::Image(img) => HttpResponse::Ok().content_type("text/html").body(
            SECRET
                .replace("{{ from }}", &data.from)
                .replace("{{ content }}", &format!("<img src='{}'>", img)),
        ),
        SecretKind::Text(text) => HttpResponse::Ok().content_type("text/html").body(
            SECRET
                .replace("{{ from }}", &data.from)
                .replace("{{ content }}", &format!("<p>{}</p>", text)),
        ),
    };
    if req.cookie("ln_viewer").is_none() {
        let cookie = Cookie::build("ln_viewer", viewer_id)
            .path("/")
            .http_only(true)
            .max_age(actix_web::cookie::time::Duration::hours(1))
            .secure(true)
            .finish();
        response.add_cookie(&cookie).ok(); // silently fail if cookie can't be added
    }

    response
}

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok().content_type("text/html").body(&**HOME)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("Starting server on http://localhost:8093/");
    let db = web::Data::new(sled::open("db")?);
    HttpServer::new(move || {
        App::new()
            .app_data(db.clone())
            .service(get_link)
            .service(create)
            .service(create_html)
            .service(index)
    })
    .bind(("127.0.0.1", 8093))?
    .run()
    .await
}
