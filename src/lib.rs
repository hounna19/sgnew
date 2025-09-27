mod common;
mod config;
mod proxy;

use crate::config::Config;
use crate::proxy::*;

use std::collections::HashMap;
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use serde_json::json;
use uuid::Uuid;
use worker::*;
use once_cell::sync::Lazy;
use regex::Regex;

static PROXYIP_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^.+:\d+$").unwrap());
// (catatan: untuk validasi local kita gunakan format "addr:port" di JSON response)

#[event(fetch)]
async fn main(req: Request, env: Env, _: Context) -> Result<Response> {
    let uuid = env
        .var("UUID")
        .map(|x| Uuid::parse_str(&x.to_string()).unwrap_or_default())?;
    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x|x.to_string()).unwrap_or_default();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x|x.to_string()).unwrap_or_default();

    let cfg = Config {
        uuid,
        host,
        main_page_url,
        sub_page_url,
    };

    Router::new()
        .get("/", index)
        .get("/siren", siren)
        .get("/sub", sub)
        .get("/vmess", get_vmess_list)    // GET mengembalikan JSON daftar proxy
        .on_async("/vmess", tunnel)       // WebSocket upgrade / tunnel ada di path yang sama
        .run(req, env, cfg)
        .await
}

async fn index(_req: Request, _cx: RouteContext<Config>) -> Result<Response> {
    Response::from_html("hi from rust!")
}

async fn siren(_req: Request, cx: RouteContext<Config>) -> Result<Response> {
    let uuid = cx.data.uuid.to_string();
    let host = cx.data.host.to_string();

    let vmess_link = {
        let config = json!(
            {"v": "2",
            "ps": "siren",
            "add": host,
            "port": "443",
            "id": uuid,
            "aid": "0",
            "net": "ws",
            "type": "none",
            "host": host,
            "path": "/KR",
            "tls": "",
            "sni": "",
            "alpn": ""}
        );
        format!("vmess://{}", URL_SAFE.encode(config.to_string()))
    };
    let vless_link = format!("vless://{uuid}@{host}:443?encryption=none&type=ws&host={host}&path=%2FKR&security=tls&sni={host}#siren vless");
    let trojan_link = format!("trojan://{uuid}@{host}:443?security=tls&type=ws&host={host}&path=%2FKR&sni={host}#siren trojan");
    let ss_link = format!("ss://{}@{host}:443?plugin=v2ray-plugin;mode=websocket;host={host};path=%2FKR#siren ss", URL_SAFE.encode(format!("none:{uuid}")));

    Response::from_body(ResponseBody::Body(format!("{vmess_link}\n{vless_link}\n{trojan_link}\n{ss_link}").into()))
}

async fn sub(_req: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.sub_page_url).await
}

/// GET /vmess
/// Mengembalikan JSON yang berisi daftar vmess proxies.
/// Saat ini hanya mengandung single entry "172.232.231.191:444"
async fn get_vmess_list(_req: Request, _cx: RouteContext<Config>) -> Result<Response> {
    let body = json!({
        "vmess": [
            "172.232.231.191:444"
        ]
    });
    Response::from_json(&body)
}

/// Tunnel handler yang dipake untuk WebSocket pada path /vmess
/// Karena kita ingin selalu pakai 1 proxy, kita override di sini.
/// Untuk kompatibilitas internal, ProxyStream/logic mengharapkan cx.data.proxy_addr & proxy_port.
async fn tunnel(req: Request, mut cx: RouteContext<Config>) -> Result<Response> {
    // override: selalu pakai proxy statis dalam bentuk "addr:port"
    let proxy_addr_port = "172.232.231.191:444".to_string();

    // set ke cx.data (struktur Config harus memiliki field proxy_addr & proxy_port)
    // jika struct Config Anda berbeda, sesuaikan penempatan nilai ini.
    if let Some((addr, port_str)) = proxy_addr_port.split_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            cx.data.proxy_addr = addr.to_string();
            cx.data.proxy_port = port;
        }
    }

    // Tangani WebSocket upgrade
    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default();
    if upgrade.to_lowercase() == "websocket" {
        // Buat pair websocket
        let WebSocketPair { server, client } = WebSocketPair::new()?;
        server.accept()?;

        // spawn local task yang memproses ProxyStream (menggunakan cx.data yang sekarang berisi proxy statis)
        wasm_bindgen_futures::spawn_local(async move {
            let events = match server.events() {
                Ok(ev) => ev,
                Err(e) => {
                    console_error!("[tunnel] failed to get events: {:?}", e);
                    return;
                }
            };

            if let Err(e) = ProxyStream::new(cx.data, &server, events).process().await {
                console_error!("[tunnel] ProxyStream error: {:?}", e);
            }
        });

        Response::from_websocket(client)
    } else {
        // Jika bukan websocket, kembalikan info sederhana (atau Anda bisa redirect)
        Response::from_html("This endpoint expects a WebSocket upgrade at /vmess")
    }
}
