use crate::client::{Comment, Post, PostDetail};
use crate::db::{CacheStats, CacheSummary, InstanceInfo};
use colored::Colorize;
use std::io::{IsTerminal, Write, stderr};
use std::sync::OnceLock;
use std::time::Instant;

fn truncate(s: &str, max_chars: usize) -> String {
    let mut end = 0;
    for (i, (idx, _)) in s.char_indices().enumerate() {
        if i >= max_chars {
            return format!("{}...", &s[..end]);
        }
        end = idx;
    }
    s.to_string()
}

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub struct Progress {
    is_tty: bool,
    total: usize,
    done: usize,
    frame: usize,
}

impl Progress {
    pub fn new(total: usize) -> Self {
        let is_tty = stderr().is_terminal();
        if is_tty {
            eprint!("\r{} Trying {} instances...", SPINNER[0], total);
            stderr().flush().ok();
        }
        Progress {
            is_tty,
            total,
            done: 0,
            frame: 0,
        }
    }

    pub fn tick(&mut self, label: &str) {
        self.done += 1;
        self.frame += 1;
        if self.is_tty {
            let ch = SPINNER[self.frame % SPINNER.len()];
            eprint!("\r{} [{}/{}] {}   ", ch, self.done, self.total, label);
            stderr().flush().ok();
        }
    }

    pub fn finish(&self) {
        if self.is_tty {
            eprint!("\r{}\r", " ".repeat(60));
            stderr().flush().ok();
        }
    }
}

pub fn print_posts(posts: &[Post], limit: usize) {
    if posts.is_empty() {
        println!("{}", "No posts found.".yellow());
        return;
    }

    println!();
    for (i, p) in posts.iter().take(limit).enumerate() {
        let num = format!("{:>3}.", i + 1).dimmed();
        let score = format!("{:>5}", p.score);
        let sticky_marker = if p.stickied { " [pinned]".yellow().to_string() } else { String::new() };

        println!(
            "{} {} {}{}",
            num,
            score.green(),
            p.title.bold(),
            sticky_marker,
        );

        let mut meta = vec![
            format!("u/{}", p.author),
            p.comments.clone(),
        ];
        if let Some(ref flair) = p.flair {
            meta.push(flair.clone());
        }
        println!("          {}", meta.join(" · ").dimmed());
        println!("          {}", p.permalink.blue());
        println!();
    }
}

pub fn print_posts_with_label(posts: &[Post], limit: usize, label: &str) {
    eprintln!("{}", format!("({})", label).dimmed());
    print_posts(posts, limit);
}

pub fn print_json(posts: &[Post]) {
    println!("{}", serde_json::to_string_pretty(posts).unwrap());
}

pub fn print_post_detail(detail: &PostDetail) {
    println!("{}", detail.title.bold());
    println!(
        "{}",
        format!(
            "r/{} · u/{} · {} · {}",
            detail.subreddit, detail.author, detail.score, detail.comment_count
        )
        .dimmed()
    );

    if !detail.body.is_empty() {
        println!();
        println!("{}", detail.body);
    }

    if !detail.comments.is_empty() {
        println!();
        println!("{}", "─".repeat(70).dimmed());

        for c in &detail.comments {
            let indent = "  ".repeat(c.depth);
            let op_marker = if c.op { " [OP]".blue().to_string() } else { String::new() };
            println!(
                "\n{}{}{} ",
                indent,
                format!("u/{}", c.author).dimmed(),
                op_marker,
            );
            if !c.body.is_empty() {
                for line in c.body.lines() {
                    println!("{}{}", indent, line);
                }
            }
        }
    }
}

pub fn print_post_json(detail: &PostDetail) {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        title: &'a str,
        author: &'a str,
        score: &'a str,
        subreddit: &'a str,
        body: &'a str,
        comments: &'a [Comment],
    }
    let out = Out {
        title: &detail.title,
        author: &detail.author,
        score: &detail.score,
        subreddit: &detail.subreddit,
        body: &detail.body,
        comments: &detail.comments,
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

pub fn print_instance_list(instances: &[InstanceInfo]) {
    if instances.is_empty() {
        println!(
            "{}",
            "No instances in registry. Run `reddit discover` to find some.".yellow()
        );
        return;
    }

    println!(
        "{:<45} {:>5} {:>5} {:>8}  {}",
        "INSTANCE".bold(),
        "OK".green().bold(),
        "FAIL".red().bold(),
        "LATENCY".bold(),
        "LAST ERROR".bold(),
    );
    println!("{}", "─".repeat(90).dimmed());

    for inst in instances {
        let latency = inst
            .avg_latency_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "—".into());
        let raw_error = inst.last_error.as_deref().unwrap_or("—");
        let error = raw_error
            .strip_prefix(&inst.url)
            .and_then(|s| s.strip_prefix(": "))
            .unwrap_or(raw_error);
        let error_display = truncate(error, 25);

        println!(
            "{:<45} {:>5} {:>5} {:>8}  {}",
            inst.url,
            inst.success_count.to_string().green(),
            inst.failure_count.to_string().red(),
            latency,
            error_display.dimmed(),
        );
    }
}

pub fn print_cache_list(entries: &[CacheSummary], stats: &CacheStats) {
    if entries.is_empty() {
        println!("{}", "Cache is empty.".yellow());
        return;
    }

    println!(
        "{} — {} entries, {}",
        "Result Cache".bold(),
        stats.entry_count,
        format_bytes(stats.db_size_bytes),
    );
    println!();

    println!(
        "{:<40} {:>7} {:>8}",
        "KEY".bold(),
        "POSTS".bold(),
        "AGE".bold(),
    );
    println!("{}", "─".repeat(58).dimmed());

    for e in entries {
        let key_display = truncate(&e.key, 38);
        println!(
            "{:<40} {:>7} {:>8}",
            key_display,
            e.result_count,
            format_duration(e.age_secs),
        );
    }
}

pub fn format_duration(secs: i64) -> String {
    if secs < 0 {
        "0s".to_string()
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

static VERBOSE_START: OnceLock<Instant> = OnceLock::new();

pub fn verbose(msg: &str) {
    let start = VERBOSE_START.get_or_init(Instant::now);
    let elapsed = start.elapsed().as_millis();
    eprintln!("{} {}", format!("[+{elapsed}ms]").dimmed(), msg);
}
