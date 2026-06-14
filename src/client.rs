use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .build()
        .expect("failed to build HTTP client")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub title: String,
    pub permalink: String,
    pub author: String,
    pub score: String,
    pub comments: String,
    pub flair: Option<String>,
    pub stickied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub score: String,
    pub op: bool,
    pub depth: usize,
}

pub struct PostDetail {
    pub title: String,
    pub author: String,
    pub score: String,
    pub comment_count: String,
    pub body: String,
    pub subreddit: String,
    pub comments: Vec<Comment>,
    pub latency_ms: u64,
}

pub struct FetchResult {
    pub posts: Vec<Post>,
    pub latency_ms: u64,
}

#[derive(Debug)]
pub enum FetchError {
    Http(u16, String),
    Network(String),
    Blocked(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Http(code, url) => write!(f, "{url}: HTTP {code}"),
            FetchError::Network(msg) => write!(f, "{msg}"),
            FetchError::Blocked(url) => write!(f, "{url}: blocked by anti-bot"),
        }
    }
}

pub async fn fetch_subreddit(
    client: &reqwest::Client,
    base_url: &str,
    subreddit: &str,
    sort: &str,
) -> Result<FetchResult, FetchError> {
    let url = format!(
        "{}/r/{}/{}",
        base_url.trim_end_matches('/'),
        subreddit,
        sort,
    );
    let start = Instant::now();

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::Network(format!("{base_url}: {e}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(FetchError::Http(status, base_url.to_string()));
    }

    let html = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(format!("{base_url}: {e}")))?;

    if html.contains("anubis_challenge") || html.contains("Making sure you&#39;re not a bot") {
        return Err(FetchError::Blocked(base_url.to_string()));
    }

    let posts = parse_posts(&html);
    let latency_ms = start.elapsed().as_millis() as u64;

    Ok(FetchResult { posts, latency_ms })
}

fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

static POST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<div class="post((?:\s[^"]*)?)"[^>]*>"#).unwrap());
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<h2 class="post_title">.*?<a href="(/r/[^/]+/comments/[^"]+)">([^<]+)</a>"#)
        .unwrap()
});
static SCORE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<div class="post_score" title="([^"]*)">"#).unwrap());
static AUTHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<a class="post_author[^"]*" href="/u/([^"]+)">"#).unwrap());
static COMMENTS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"class="post_comments" title="([^"]*)">"#).unwrap());
static FLAIR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<p class="post_flair[^"]*">([^<]+)</p>"#).unwrap());

pub async fn search(
    client: &reqwest::Client,
    base_url: &str,
    query: &str,
    subreddit: Option<&str>,
    sort: &str,
    time: &str,
) -> Result<FetchResult, FetchError> {
    let base = base_url.trim_end_matches('/');
    let url = match subreddit {
        Some(sub) => format!("{base}/r/{sub}/search"),
        None => format!("{base}/search"),
    };
    let start = Instant::now();

    let restrict = if subreddit.is_some() { "on" } else { "" };
    let resp = client
        .get(&url)
        .query(&[
            ("q", query),
            ("sort", sort),
            ("t", time),
            ("restrict_sr", restrict),
        ])
        .send()
        .await
        .map_err(|e| FetchError::Network(format!("{base_url}: {e}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(FetchError::Http(status, base_url.to_string()));
    }

    let html = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(format!("{base_url}: {e}")))?;

    if html.contains("anubis_challenge") || html.contains("Making sure you&#39;re not a bot") {
        return Err(FetchError::Blocked(base_url.to_string()));
    }

    let posts = parse_posts(&html);
    let latency_ms = start.elapsed().as_millis() as u64;

    Ok(FetchResult { posts, latency_ms })
}

pub async fn fetch_post(
    client: &reqwest::Client,
    base_url: &str,
    permalink: &str,
) -> Result<PostDetail, FetchError> {
    let base = base_url.trim_end_matches('/');
    let path = permalink.trim_end_matches('/');
    let url = format!("{base}{path}");
    let start = Instant::now();

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::Network(format!("{base_url}: {e}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(FetchError::Http(status, base_url.to_string()));
    }

    let html = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(format!("{base_url}: {e}")))?;

    if html.contains("anubis_challenge") || html.contains("Making sure you&#39;re not a bot") {
        return Err(FetchError::Blocked(base_url.to_string()));
    }

    let latency_ms = start.elapsed().as_millis() as u64;

    let title = TITLE_RE
        .captures(&html)
        .map(|c| html_decode(&c[2]))
        .unwrap_or_default();
    let author = AUTHOR_RE
        .captures(&html)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let score = SCORE_RE
        .captures(&html)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let comment_count = COMMENTS_RE
        .captures(&html)
        .map(|c| c[1].to_string())
        .unwrap_or_default();

    let subreddit = permalink
        .strip_prefix("/r/")
        .and_then(|s| s.split('/').next())
        .unwrap_or("")
        .to_string();

    static BODY_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<div class="post_body">(.*?)</div>"#).unwrap()
    });
    let body = BODY_RE
        .captures(&html)
        .map(|c| html_decode(&strip_tags(&c[1])))
        .unwrap_or_default();

    let comments = parse_comments(&html);

    Ok(PostDetail {
        title,
        author,
        score,
        comment_count,
        body,
        subreddit,
        comments,
        latency_ms,
    })
}

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

fn strip_tags(s: &str) -> String {
    TAG_RE.replace_all(s, "").trim().to_string()
}

fn parse_comments(html: &str) -> Vec<Comment> {
    static CMT_AUTHOR_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<a class="comment_author\s*([^"]*)" href="/user/([^"]+)">"#).unwrap()
    });
    static CMT_BODY_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<div class="comment_body[^"]*">(.*?)</div>"#).unwrap()
    });

    let mut comments = Vec::new();
    let mut depth: i32 = 0;
    let mut pos = html.find("id=\"comments\"").unwrap_or(0);

    while pos < html.len() {
        let remaining = &html[pos..];

        let next_open = remaining.find("<details class=\"comment\"");
        let next_author = CMT_AUTHOR_RE.find(remaining).map(|m| m.start());
        let next_close = remaining.find("</details>");

        let events: Vec<(usize, u8)> = [
            next_open.map(|p| (p, 0u8)),
            next_author.map(|p| (p, 1)),
            next_close.map(|p| (p, 2)),
        ]
        .into_iter()
        .flatten()
        .collect();

        let Some(&(offset, event_type)) = events.iter().min_by_key(|(p, _)| *p) else {
            break;
        };

        match event_type {
            0 => {
                depth += 1;
                pos += offset + 1;
            }
            1 => {
                let caps = CMT_AUTHOR_RE.captures(remaining).unwrap();
                let classes = caps[1].to_string();
                let author = caps[2].to_string();
                let op = classes.contains("op");
                let current_depth = (depth - 1).max(0) as usize;

                let after_author = &html[pos + offset..];
                let body = CMT_BODY_RE
                    .captures(after_author)
                    .map(|c| html_decode(&strip_tags(&c[1])))
                    .unwrap_or_default();

                comments.push(Comment {
                    author,
                    body,
                    score: String::new(),
                    op,
                    depth: current_depth,
                });

                pos += offset + caps[0].len();
            }
            2 => {
                depth -= 1;
                pos += offset + "</details>".len();
            }
            _ => break,
        }
    }

    comments
}

fn parse_posts(html: &str) -> Vec<Post> {
    let post_starts: Vec<(usize, bool)> = POST_RE
        .captures_iter(html)
        .map(|c| {
            let m = c.get(0).unwrap();
            let classes = c.get(1).map(|m| m.as_str()).unwrap_or("");
            (m.start(), classes.contains("stickied"))
        })
        .collect();

    let mut posts = Vec::new();

    for (idx, &(start, stickied)) in post_starts.iter().enumerate() {
        let end = post_starts
            .get(idx + 1)
            .map(|&(s, _)| s)
            .unwrap_or(html.len());
        let block = &html[start..end];

        let title_cap = match TITLE_RE.captures(block) {
            Some(c) => c,
            None => continue,
        };
        let permalink = title_cap[1].to_string();
        let title = html_decode(&title_cap[2]);

        let score = SCORE_RE
            .captures(block)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        let author = AUTHOR_RE
            .captures(block)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        let comments = COMMENTS_RE
            .captures(block)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        let flair = FLAIR_RE.captures(block).map(|c| html_decode(&c[1]));

        posts.push(Post {
            title,
            permalink,
            author,
            score,
            comments,
            flair,
            stickied,
        });
    }

    posts
}
