use crate::client;
use crate::db::Db;
use crate::display;
use colored::Colorize;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[derive(Debug)]
pub struct ProbeResult {
    pub url: String,
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub post_count: Option<usize>,
}

pub async fn discover(client: &reqwest::Client, db: &Db, verbose: bool) {
    if verbose {
        display::verbose("fetching instance list from GitHub...");
    }

    let urls = match fetch_instance_list(client).await {
        Ok(urls) => {
            if verbose {
                display::verbose(&format!("got {} instances", urls.len()));
            }
            urls
        }
        Err(e) => {
            eprintln!("{}: {e}", "error".red().bold());
            eprintln!("falling back to known instances...");
            db.all_instances().into_iter().map(|i| i.url).collect()
        }
    };

    if urls.is_empty() {
        println!("{}", "No instances to probe.".yellow());
        return;
    }

    println!("Probing {} instances...\n", urls.len());

    let sem = Arc::new(Semaphore::new(10));
    let mut set = JoinSet::new();

    for u in urls {
        let c = client.clone();
        let s = sem.clone();
        set.spawn(async move {
            let _permit = s.acquire().await.unwrap();
            probe_instance(&c, &u).await
        });
    }

    let total = set.len();
    let mut results: Vec<ProbeResult> = Vec::new();
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    while let Some(Ok(result)) = set.join_next().await {
        results.push(result);
        if is_tty && total > 1 {
            let ok = results.iter().filter(|r| r.ok).count();
            eprint!(
                "\r[{}/{}] {} working so far...   ",
                results.len(),
                total,
                ok
            );
            std::io::Write::flush(&mut std::io::stderr()).ok();
        }
    }
    if is_tty && total > 1 {
        eprint!("\r{}\r", " ".repeat(50));
        std::io::Write::flush(&mut std::io::stderr()).ok();
    }

    results.sort_by(|a, b| {
        b.ok.cmp(&a.ok).then_with(|| {
            a.latency_ms
                .unwrap_or(u64::MAX)
                .cmp(&b.latency_ms.unwrap_or(u64::MAX))
        })
    });

    let ok_count = results.iter().filter(|r| r.ok).count();

    println!(
        "{:<50} {:>7} {:>8}  {}",
        "INSTANCE".bold(),
        "STATUS".bold(),
        "LATENCY".bold(),
        "DETAILS".bold(),
    );
    println!("{}", "─".repeat(85).dimmed());

    for r in &results {
        let status = if r.ok {
            "OK".green().to_string()
        } else {
            "FAIL".red().to_string()
        };
        let latency = r
            .latency_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "—".into());
        let detail = if r.ok {
            format!("{} posts", r.post_count.unwrap_or(0))
        } else {
            r.error.clone().unwrap_or_default()
        };
        let detail_truncated = if detail.len() > 35 {
            format!("{}...", &detail[..35])
        } else {
            detail
        };

        println!(
            "{:<50} {:>7} {:>8}  {}",
            r.url,
            status,
            latency,
            detail_truncated.dimmed(),
        );

        if r.ok {
            db.add_instance(&r.url);
            db.record_success(&r.url, r.latency_ms.unwrap_or(0));
        } else {
            db.add_instance(&r.url);
            db.record_failure(&r.url, detail_truncated.trim());
            if r.error
                .as_deref()
                .is_some_and(|e| e.contains("anti-bot"))
            {
                db.mark_unreachable(&r.url);
            }
        }
    }

    println!(
        "\n{} {ok_count}/{} instances reachable from CLI",
        "Done.".green().bold(),
        results.len(),
    );
}

async fn probe_instance(client: &reqwest::Client, base_url: &str) -> ProbeResult {
    match client::fetch_subreddit(client, base_url, "all", "hot").await {
        Ok(fr) => ProbeResult {
            url: base_url.to_string(),
            ok: true,
            latency_ms: Some(fr.latency_ms),
            error: None,
            post_count: Some(fr.posts.len()),
        },
        Err(e) => ProbeResult {
            url: base_url.to_string(),
            ok: false,
            latency_ms: None,
            error: Some(e.to_string()),
            post_count: None,
        },
    }
}

#[derive(Deserialize)]
struct InstanceList {
    instances: Vec<InstanceEntry>,
}

#[derive(Deserialize)]
struct InstanceEntry {
    url: Option<String>,
}

async fn fetch_instance_list(client: &reqwest::Client) -> Result<Vec<String>, String> {
    let resp = client
        .get("https://raw.githubusercontent.com/redlib-org/redlib-instances/main/instances.json")
        .send()
        .await
        .map_err(|e| format!("failed to fetch instance list: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub returned HTTP {}", resp.status()));
    }

    let list: InstanceList = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse instance list: {e}"))?;

    Ok(list
        .instances
        .into_iter()
        .filter_map(|e| e.url)
        .map(|u| u.trim_end_matches('/').to_string())
        .collect())
}
