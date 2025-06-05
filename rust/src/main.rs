use clap::Parser;
use reqwest::blocking::Client;
use reqwest::header::USER_AGENT;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::net::{TcpListener, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tiny_http::{Response, Server};

#[derive(Debug, Deserialize)]
struct Config {
    proxy: Proxy,
    analyzer: Analyzer,
}

#[derive(Debug, Deserialize)]
struct Proxy {
    port: u16,
    #[serde(rename = "backend-url")]
    backend_url: String,
}

#[derive(Debug, Deserialize)]
struct Analyzer {
    port: u16,
    #[serde(rename = "max-examples")]
    max_examples: usize,
    #[serde(rename = "redacted-fields", default)]
    redacted_fields: Vec<String>,
    storage: Storage,
}

#[derive(Debug, Deserialize)]
struct Storage {
    path: String,
    frequency: u64,
}

#[derive(Default, Serialize)]
struct SchemaStore {
    examples: HashMap<String, Vec<String>>, // path -> examples
    optional: HashMap<String, bool>,         // path -> optional
}

impl SchemaStore {
    fn add_value(&mut self, path: &str, value: String, max: usize) {
        let entry = self.examples.entry(path.to_string()).or_insert_with(Vec::new);
        if entry.len() < max && !entry.contains(&value) {
            entry.push(value);
        }
    }

    fn set_optional(&mut self, path: &str, opt: bool) {
        self.optional.insert(path.to_string(), opt);
    }
}

#[derive(Default, Serialize)]
struct ResponseData {
    headers: SchemaStore,
    payload: SchemaStore,
}

#[derive(Default, Serialize)]
struct EndpointData {
    method: String,
    url: String,
    request_headers: SchemaStore,
    request_payload: SchemaStore,
    url_parameters: SchemaStore,
    response_statuses: HashMap<u16, ResponseData>,
}

#[derive(Default, Serialize)]
struct AnalyzerStore {
    endpoints: HashMap<String, EndpointData>,
    max_examples: usize,
}

impl AnalyzerStore {
    fn new(max_examples: usize) -> Self {
        AnalyzerStore { endpoints: HashMap::new(), max_examples }
    }

    fn process_request(&mut self, method: &str, path: &str, status: u16) {
        let key = format!("{} {}", method, path);
        let ep = self.endpoints.entry(key.clone()).or_insert_with(|| EndpointData {
            method: method.to_string(),
            url: path.to_string(),
            ..Default::default()
        });

        ep.response_statuses.entry(status).or_insert_with(ResponseData::default);
    }

    fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Path to configuration file
    #[arg(short, long)]
    config: String,
}

fn load_config(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(path)?;
    let cfg: Config = serde_yaml::from_str(&data)?;
    Ok(cfg)
}

fn check_port_available(port: u16) -> Result<(), String> {
    let addr = format!("127.0.0.1:{}", port);
    TcpListener::bind(addr)
        .map(|_| ())
        .map_err(|e| format!("port {} unavailable: {}", port, e))
}

fn checkBackendReachable(url: &str) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let res = client
        .head(url)
        .header(USER_AGENT, "docurift-rs")
        .send()
        .map_err(|e| e.to_string())?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(format!("backend returned status {}", res.status()))
    }
}

fn start_analyzer_server(port: u16, store: Arc<Mutex<AnalyzerStore>>) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("0.0.0.0:{}", port);
    let server = Server::http(addr)?;
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let path = request.url().to_string();
            if path == "/api/analyzer" {
                let data = store.lock().unwrap().to_json();
                let header = tiny_http::Header::from_bytes(
                    &b"Content-Type"[..],
                    &b"application/json"[..],
                )
                .unwrap();
                let response = Response::from_string(data).with_header(header);
                let _ = request.respond(response);
            } else {
                let response = Response::from_string("Not Found").with_status_code(404);
                let _ = request.respond(response);
            }
        }
    });
    Ok(())
}

fn start_proxy(cfg: Proxy, store: Arc<Mutex<AnalyzerStore>>) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("0.0.0.0:{}", cfg.port);
    let server = Server::http(addr)?;
    let client = Client::builder().build().map_err(|e| e.to_string())?;
    for mut req in server.incoming_requests() {
        let method = req.method().as_str().to_string();
        let url = format!("{}{}", cfg.backend_url, req.url());
        let mut body = Vec::new();
        req.as_reader().read_to_end(&mut body)?;
        let req_method = method.parse::<reqwest::Method>()?;
        let mut builder = client.request(req_method, &url);
        for h in req.headers() {
            builder = builder.header(h.field.as_str(), h.value.as_str());
        }
        if !body.is_empty() {
            builder = builder.body(body.clone());
        }
        match builder.send() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let resp_body = resp.text().unwrap_or_default();
                let mut response = Response::from_string(resp_body);
                response = response.with_status_code(status);
                for h in resp.headers() {
                    if let Ok(header) = tiny_http::Header::from_bytes(
                        h.name.as_str().as_bytes(),
                        h.value.as_str().as_bytes(),
                    ) {
                        response.add_header(header);
                    }
                }
                let _ = req.respond(response);
                store.lock().unwrap().process_request(&method, req.url(), status);
            }
            Err(e) => {
                let response = Response::from_string(format!("Bad Gateway: {}", e)).with_status_code(502);
                let _ = req.respond(response);
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cfg = load_config(&args.config)?;

    check_port_available(cfg.proxy.port)?;
    check_port_available(cfg.analyzer.port)?;

    if let Err(e) = checkBackendReachable(&cfg.proxy.backend_url) {
        eprintln!("Warning: backend {} unreachable: {}", cfg.proxy.backend_url, e);
    }

    println!("Proxy on port {} forwarding to {}", cfg.proxy.port, cfg.proxy.backend_url);
    println!("Analyzer port {}", cfg.analyzer.port);

    let store = Arc::new(Mutex::new(AnalyzerStore::new(cfg.analyzer.max_examples)));

    start_analyzer_server(cfg.analyzer.port, store.clone())?;
    start_proxy(cfg.proxy, store)?;

    Ok(())
}
