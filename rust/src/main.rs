use clap::Parser;
use reqwest::blocking::Client;
use reqwest::header::USER_AGENT;
use serde::Deserialize;
use std::fs;
use std::net::{TcpListener, ToSocketAddrs};
use std::time::Duration;

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

    // TODO: implement HTTP proxy and analyzer logic

    Ok(())
}
