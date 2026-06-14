use crate::client;
use crate::db::Db;
use crate::display;
use colored::Colorize;
use serde::Deserialize;
use std::collections::HashSet;
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
    pub source: String,
}

#[derive(Debug, Clone)]
struct DiscoveredInstance {
    url: String,
    source: String,
}

#[derive(Debug, Clone)]
pub enum DiscoverMode {
    Quick,
    Shodan,
    Searx,
    Deep,
}

pub async fn discover(client: &reqwest::Client, db: &Db, verbose: bool, mode: DiscoverMode) {
    let mut all: Vec<DiscoveredInstance> = Vec::new();

    // Phase 1: curated lists (always, in parallel)
    if verbose {
        display::verbose("fetching curated instance lists...");
    }

    let (github, codeberg, farside) = tokio::join!(
        fetch_instance_json(
            client,
            "https://raw.githubusercontent.com/redlib-org/redlib-instances/main/instances.json",
            "github",
        ),
        fetch_instance_json(
            client,
            "https://codeberg.org/mysearchhistory123/redlib-instances/raw/branch/main/instances.json",
            "codeberg",
        ),
        fetch_farside_list(client),
    );

    if verbose {
        display::verbose(&format!(
            "curated: {} github, {} codeberg, {} farside",
            github.len(),
            codeberg.len(),
            farside.len()
        ));
    }

    all.extend(github);
    all.extend(codeberg);
    all.extend(farside);

    // Phase 2: optional sources
    match mode {
        DiscoverMode::Shodan | DiscoverMode::Deep => {
            if verbose {
                display::verbose("scanning Shodan for unlisted instances...");
            }
            let shodan = discover_shodan(verbose).await;
            if verbose {
                display::verbose(&format!("shodan: {} results", shodan.len()));
            }
            all.extend(shodan);
        }
        _ => {}
    }

    match mode {
        DiscoverMode::Searx | DiscoverMode::Deep => {
            if verbose {
                display::verbose("searching web for instance lists...");
            }
            let searx = discover_searx(verbose).await;
            if verbose {
                display::verbose(&format!("searx: {} results", searx.len()));
            }
            all.extend(searx);
        }
        _ => {}
    }

    // Phase 3: fallback to known instances if nothing found
    if all.is_empty() {
        eprintln!("falling back to known instances...");
        all = db
            .all_instances()
            .into_iter()
            .map(|i| DiscoveredInstance {
                url: i.url,
                source: "db".into(),
            })
            .collect();
    }

    // Deduplicate by normalized URL
    let mut seen = HashSet::new();
    all.retain(|d| {
        let key = d.url.trim_end_matches('/').to_lowercase();
        seen.insert(key)
    });

    // Filter .onion
    all.retain(|d| !d.url.contains(".onion"));

    if all.is_empty() {
        println!("{}", "No instances to probe.".yellow());
        return;
    }

    let source_count = {
        let sources: HashSet<&str> = all.iter().map(|d| d.source.as_str()).collect();
        sources.len()
    };

    println!(
        "Probing {} instances from {} source{}...\n",
        all.len(),
        source_count,
        if source_count == 1 { "" } else { "s" },
    );

    // Phase 4: probe all instances
    let sem = Arc::new(Semaphore::new(10));
    let mut set = JoinSet::new();

    for d in all {
        let c = client.clone();
        let s = sem.clone();
        set.spawn(async move {
            let _permit = s.acquire().await.unwrap();
            probe_instance(&c, &d.url, &d.source).await
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

    // Phase 5: display results
    println!(
        "{:<50} {:<10} {:>7} {:>8}  {}",
        "INSTANCE".bold(),
        "SOURCE".bold(),
        "STATUS".bold(),
        "LATENCY".bold(),
        "DETAILS".bold(),
    );
    println!("{}", "─".repeat(95).dimmed());

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
            "{:<50} {:<10} {:>7} {:>8}  {}",
            r.url,
            r.source.dimmed(),
            status,
            latency,
            detail_truncated.dimmed(),
        );

        if r.ok {
            db.add_instance(&r.url, Some(&r.source));
            db.record_success(&r.url, r.latency_ms.unwrap_or(0));
        } else {
            db.add_instance(&r.url, Some(&r.source));
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

async fn probe_instance(client: &reqwest::Client, base_url: &str, source: &str) -> ProbeResult {
    match client::fetch_subreddit(client, base_url, "all", "hot").await {
        Ok(fr) => ProbeResult {
            url: base_url.to_string(),
            ok: true,
            latency_ms: Some(fr.latency_ms),
            error: None,
            post_count: Some(fr.posts.len()),
            source: source.to_string(),
        },
        Err(e) => ProbeResult {
            url: base_url.to_string(),
            ok: false,
            latency_ms: None,
            error: Some(e.to_string()),
            post_count: None,
            source: source.to_string(),
        },
    }
}

// --- Curated list sources ---

#[derive(Deserialize)]
struct InstanceList {
    instances: Vec<InstanceEntry>,
}

#[derive(Deserialize)]
struct InstanceEntry {
    url: Option<String>,
}

async fn fetch_instance_json(
    client: &reqwest::Client,
    list_url: &str,
    source: &str,
) -> Vec<DiscoveredInstance> {
    let resp = match client.get(list_url).send().await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    if !resp.status().is_success() {
        return Vec::new();
    }

    let list: InstanceList = match resp.json().await {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };

    list.instances
        .into_iter()
        .filter_map(|e| e.url)
        .map(|u| DiscoveredInstance {
            url: u.trim_end_matches('/').to_string(),
            source: source.to_string(),
        })
        .collect()
}

#[derive(Deserialize)]
struct FarsideService {
    #[serde(rename = "type")]
    service_type: String,
    #[serde(default)]
    instances: Vec<String>,
}

async fn fetch_farside_list(client: &reqwest::Client) -> Vec<DiscoveredInstance> {
    let resp = match client
        .get("https://raw.githubusercontent.com/benbusby/farside/main/services.json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    if !resp.status().is_success() {
        return Vec::new();
    }

    let services: Vec<FarsideService> = match resp.json().await {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    services
        .into_iter()
        .filter(|s| s.service_type == "redlib" || s.service_type == "libreddit")
        .flat_map(|s| s.instances)
        .map(|u| DiscoveredInstance {
            url: u.trim_end_matches('/').to_string(),
            source: "farside".to_string(),
        })
        .collect()
}

// --- CLI tool sources ---

async fn discover_shodan(verbose: bool) -> Vec<DiscoveredInstance> {
    if tokio::process::Command::new("which")
        .arg("shodan")
        .output()
        .await
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!(
            "{}",
            "shodan CLI not found, skipping Shodan scan".yellow()
        );
        return Vec::new();
    }

    let queries = &[
        r#"http.html:"alternative private front-end to Reddit""#,
        r#"http.title:"Redlib""#,
    ];

    let mut all = Vec::new();

    for query in queries {
        if verbose {
            display::verbose(&format!("shodan: {query}"));
        }

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new("shodan")
                .args(["search", "--fields", "ip_str,port", "--limit", "100", query])
                .output(),
        )
        .await
        {
            Ok(Ok(o)) => o,
            _ => continue,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 2 {
                let ip = parts[0].trim();
                if let Ok(port) = parts[1].trim().parse::<u16>() {
                    let url = if port == 443 {
                        format!("https://{ip}")
                    } else {
                        format!("http://{ip}:{port}")
                    };
                    all.push(DiscoveredInstance {
                        url,
                        source: "shodan".to_string(),
                    });
                }
            }
        }
    }

    // Deduplicate within shodan results
    let mut seen = HashSet::new();
    all.retain(|d| seen.insert(d.url.clone()));
    all
}

async fn discover_searx(verbose: bool) -> Vec<DiscoveredInstance> {
    if tokio::process::Command::new("which")
        .arg("searx")
        .output()
        .await
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("{}", "searx CLI not found, skipping web search".yellow());
        return Vec::new();
    }

    let queries = &["redlib instances list", "redlib alternative reddit frontend working instances"];
    let mut all = Vec::new();

    for query in queries {
        if verbose {
            display::verbose(&format!("searx: {query}"));
        }

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new("searx")
                .args(["--json", "-n", "20", query])
                .output(),
        )
        .await
        {
            Ok(Ok(o)) => o,
            _ => continue,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let results: Vec<SearxResult> = match serde_json::from_str(&stdout) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for r in results {
            if looks_like_instance(&r.url) {
                let url = r.url.trim_end_matches('/').to_string();
                all.push(DiscoveredInstance {
                    url,
                    source: "searx".to_string(),
                });
            }
        }
    }

    let mut seen = HashSet::new();
    all.retain(|d| seen.insert(d.url.clone()));
    all
}

#[derive(Deserialize)]
struct SearxResult {
    #[serde(default)]
    url: String,
}

fn looks_like_instance(url: &str) -> bool {
    let dominated = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    let dominated = dominated.split('/').next().unwrap_or(dominated);

    let keywords = ["redlib", "libreddit", "safereddit", "reddit"];
    keywords.iter().any(|kw| dominated.contains(kw))
        && !dominated.contains("github.com")
        && !dominated.contains("reddit.com")
        && !dominated.contains("codeberg.org")
}
