use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ureq::Agent;

const ATTEMPTS: u32 = 3;
const LONGEST_WAIT: Duration = Duration::from_secs(60);

pub struct GitHub {
    agent: Agent,
    api: String,
    repo: String,
    token: String,
    backoff: Duration,
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        let text = String::from_utf8_lossy(&self.body);
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.into_owned()))
    }
}

// How long to wait before the next attempt, or None when this answer stands:
// rate limits say when they lift, server errors back off 1, 2, 4 s.
fn retry_wait(
    status: u16,
    retry_after: Option<&str>,
    remaining: Option<&str>,
    reset: Option<&str>,
    attempt: u32,
    backoff: Duration,
) -> Option<Duration> {
    let rate_limited =
        status == 429 || (status == 403 && (retry_after.is_some() || remaining == Some("0")));
    if !rate_limited && !matches!(status, 500 | 502 | 503 | 504) {
        return None;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let told = retry_after
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| {
            (remaining == Some("0"))
                .then(|| {
                    reset?
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .map(|r| r.saturating_sub(now))
                })
                .flatten()
        })
        .map(Duration::from_secs);
    let wait = told.unwrap_or(backoff * 2u32.pow(attempt));
    (wait <= LONGEST_WAIT).then_some(wait)
}

impl GitHub {
    pub fn new(api: &str, repo: &str, token: &str) -> Self {
        // A connection that stalls must not hold the job until its timeout.
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .timeout_global(Some(Duration::from_secs(1800)))
            .build()
            .new_agent();
        GitHub {
            agent,
            api: api.trim_end_matches('/').into(),
            repo: repo.into(),
            token: token.into(),
            backoff: Duration::from_secs(1),
        }
    }

    // Retries network errors, server errors and rate limits. A request that
    // reached GitHub before the connection broke may run twice: the callers
    // that create things treat "already exists" as done.
    fn send(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Reply, String> {
        let mut attempt = 0;
        loop {
            let mut req = ureq::http::Request::builder()
                .method(method)
                .uri(url)
                .header("Accept", "application/vnd.github+json")
                .header("Authorization", format!("Bearer {}", self.token))
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("User-Agent", "relcut");
            if let Some(ct) = content_type {
                req = req.header("Content-Type", ct);
            }
            let sent = if content_type.is_none() && body.is_empty() {
                self.agent.run(req.body(()).map_err(|e| e.to_string())?)
            } else {
                self.agent
                    .run(req.body(body.to_vec()).map_err(|e| e.to_string())?)
            };
            let wait = match sent {
                Ok(mut res) => {
                    let status = res.status().as_u16();
                    let header = |name: &str| {
                        res.headers()
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string)
                    };
                    let (retry_after, remaining, reset) = (
                        header("retry-after"),
                        header("x-ratelimit-remaining"),
                        header("x-ratelimit-reset"),
                    );
                    let body = res
                        .body_mut()
                        .with_config()
                        .limit(u64::MAX)
                        .read_to_vec()
                        .map_err(|e| format!("{method} {url}: {e}"))?;
                    match retry_wait(
                        status,
                        retry_after.as_deref(),
                        remaining.as_deref(),
                        reset.as_deref(),
                        attempt,
                        self.backoff,
                    ) {
                        Some(wait) if attempt + 1 < ATTEMPTS => {
                            crate::log::retry(&format!(
                                "{method} {url}: {status}, again in {}s",
                                wait.as_secs()
                            ));
                            wait
                        }
                        _ => return Ok(Reply { status, body }),
                    }
                }
                Err(e) if attempt + 1 < ATTEMPTS => {
                    let wait = self.backoff * 2u32.pow(attempt);
                    crate::log::retry(&format!(
                        "{method} {url}: {e}, again in {}s",
                        wait.as_secs()
                    ));
                    wait
                }
                Err(e) => return Err(format!("{method} {url}: {e}")),
            };
            std::thread::sleep(wait);
            attempt += 1;
        }
    }

    fn call(&self, method: &str, url: &str, body: Option<Value>) -> Result<(u16, Value), String> {
        let reply = match body {
            Some(b) => self.send(
                method,
                url,
                Some("application/json"),
                b.to_string().as_bytes(),
            )?,
            None => self.send(method, url, None, &[])?,
        };
        Ok((reply.status, reply.json()))
    }

    fn ok(&self, method: &str, url: &str, body: Option<Value>) -> Result<Value, String> {
        match self.call(method, url, body)? {
            (200..=299, v) => Ok(v),
            (status, v) => Err(format!("GitHub {method} {url}: {status} {v}")),
        }
    }

    fn release_of(&self, tag: &str) -> Result<Option<Value>, String> {
        let url = format!("{}/repos/{}/releases?per_page=100", self.api, self.repo);
        let recent = self.ok("GET", &url, None)?;
        let found = recent.as_array().into_iter().flatten();
        Ok(found.into_iter().find(|r| r["tag_name"] == tag).cloned())
    }

    // The release of a tag and whether an earlier run left it, as a draft or
    // published; that one stays as it is. A new one is a draft when asked,
    // for publish_release to make public.
    pub fn release(
        &self,
        tag: &str,
        title: &str,
        body: &str,
        draft: bool,
        latest: &str,
    ) -> Result<(Value, bool), String> {
        if let Some(found) = self.release_of(tag)? {
            return Ok((found, true));
        }
        let base = format!("{}/repos/{}/releases", self.api, self.repo);
        let mut payload = json!({"tag_name": tag, "name": title, "body": body});
        if draft {
            payload["draft"] = json!(true);
        } else {
            payload["make_latest"] = json!(latest);
        }
        match self.call("POST", &base, Some(payload))? {
            (201, v) => Ok((v, false)),
            // A request that got through before its connection broke.
            (422, v) => match self.release_of(tag)? {
                Some(found) => Ok((found, true)),
                None => Err(format!("GitHub POST {base}: 422 {v}")),
            },
            (status, v) => Err(format!("GitHub POST {base}: {status} {v}")),
        }
    }

    pub fn publish_release(
        &self,
        release: &Value,
        body: &str,
        latest: &str,
    ) -> Result<Value, String> {
        let url = format!(
            "{}/repos/{}/releases/{}",
            self.api, self.repo, release["id"]
        );
        let payload = json!({"draft": false, "body": body, "make_latest": latest});
        self.ok("PATCH", &url, Some(payload))
    }

    // The title and summary GitHub shows for a check, e.g. a job's own.
    pub fn update_check_run(&self, id: &str, title: &str, summary: &str) -> Result<(), String> {
        let url = format!("{}/repos/{}/check-runs/{id}", self.api, self.repo);
        let output = json!({"output": {"title": title, "summary": summary}});
        self.ok("PATCH", &url, Some(output)).map(|_| ())
    }

    pub fn pull_requests(&self, sha: &str) -> Result<Vec<u64>, String> {
        let pulls = self.ok(
            "GET",
            &format!("{}/repos/{}/commits/{sha}/pulls", self.api, self.repo),
            None,
        )?;
        Ok(pulls
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| !p["merged_at"].is_null())
            .filter_map(|p| p["number"].as_u64())
            .collect())
    }

    // Ok(false) when `once` finds the comment there already.
    pub fn comment(&self, number: u64, body: &str, once: bool) -> Result<bool, String> {
        let url = format!("{}/repos/{}/issues/{number}/comments", self.api, self.repo);
        if once {
            let comments = self.ok("GET", &format!("{url}?per_page=100"), None)?;
            if comments
                .as_array()
                .into_iter()
                .flatten()
                .any(|c| c["body"] == body)
            {
                return Ok(false);
            }
        }
        self.ok("POST", &url, Some(json!({"body": body})))
            .map(|_| true)
    }

    // The zip of an artifact this workflow run uploaded, saved as <name>.zip.
    // The download redirects to storage, which never gets the token.
    pub fn download_artifact(
        &self,
        run_id: &str,
        name: &str,
        dest: &Path,
    ) -> Result<std::path::PathBuf, String> {
        let list = self.ok(
            "GET",
            &format!(
                "{}/repos/{}/actions/runs/{run_id}/artifacts?name={}",
                self.api,
                self.repo,
                encode(name)
            ),
            None,
        )?;
        let artifact = list["artifacts"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or(format!("no artifact {name} in run {run_id}"))?;
        let url = artifact["archive_download_url"]
            .as_str()
            .ok_or(format!("artifact {name} has no download url"))?;
        let reply = self.send("GET", url, None, &[])?;
        if !(200..300).contains(&reply.status) {
            return Err(format!("download {name}: {}", reply.status));
        }
        std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        let file = dest.join(format!("{name}.zip"));
        std::fs::write(&file, reply.body).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(file)
    }

    // An asset of the same name and size stays (Ok(.., false)); any other of
    // that name is replaced, so a re-run or a retried upload that had already
    // landed ends with the one file.
    pub fn upload(
        &self,
        release: &Value,
        asset: &crate::assets::Asset,
    ) -> Result<(Value, bool), String> {
        let name = asset.name.as_str();
        let file = &asset.file;
        let upload_url = release["upload_url"]
            .as_str()
            .ok_or("release without upload_url")?;
        let mut url = format!(
            "{}?name={}",
            upload_url.split('{').next().unwrap_or(upload_url),
            encode(name)
        );
        if let Some(label) = &asset.label {
            url.push_str(&format!("&label={}", encode(label)));
        }
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let mut refused = String::new();
        for _ in 0..2 {
            let assets = self.ok(
                "GET",
                &format!(
                    "{}/repos/{}/releases/{}/assets?per_page=100",
                    self.api, self.repo, release["id"]
                ),
                None,
            )?;
            for existing in assets
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| a["name"] == name)
            {
                if existing["state"] == "uploaded" && existing["size"] == bytes.len() {
                    return Ok((existing.clone(), false));
                }
                self.ok(
                    "DELETE",
                    &format!(
                        "{}/repos/{}/releases/assets/{}",
                        self.api, self.repo, existing["id"]
                    ),
                    None,
                )?;
            }
            let reply = self.send("POST", &url, Some(&asset.content_type), &bytes)?;
            match reply.status {
                200..=299 => return Ok((reply.json(), true)),
                422 => refused = reply.json().to_string(),
                status => return Err(format!("upload {name}: {status} {}", reply.json())),
            }
        }
        Err(format!("upload {name}: 422 {refused}"))
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn retries_server_errors_and_rate_limits_only() {
        let b = Duration::from_secs(1);
        assert_eq!(
            retry_wait(502, None, None, None, 0, b),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            retry_wait(503, None, None, None, 2, b),
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            retry_wait(429, Some("7"), None, None, 0, b),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_wait(403, Some("3"), None, None, 0, b),
            Some(Duration::from_secs(3))
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let soon = (now + 10).to_string();
        assert!(
            retry_wait(403, None, Some("0"), Some(&soon), 0, b)
                .is_some_and(|w| w <= Duration::from_secs(10))
        );
        let late = (now + 3600).to_string();
        assert_eq!(
            retry_wait(403, None, Some("0"), Some(&late), 0, b),
            None,
            "an hour is too long to wait"
        );
        for status in [200, 201, 400, 401, 403, 404, 422] {
            assert_eq!(
                retry_wait(status, None, Some("10"), None, 0, b),
                None,
                "{status}"
            );
        }
    }

    // A server that answers each connection with the next canned response.
    fn serve(responses: Vec<&'static str>) -> (String, Arc<Mutex<u32>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(Mutex::new(0));
        let seen = count.clone();
        std::thread::spawn(move || {
            for (stream, response) in listener.incoming().zip(responses) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                reader.take(length).read_to_end(&mut Vec::new()).unwrap();
                *seen.lock().unwrap() += 1;
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, count)
    }

    #[test]
    fn a_request_that_fails_twice_succeeds_on_the_third_attempt() {
        let (url, count) = serve(vec![
            "HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            "HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            "HTTP/1.1 201 Created\r\ncontent-length: 8\r\nconnection: close\r\n\r\n{\"id\":1}",
        ]);
        let mut gh = GitHub::new(&url, "o/r", "t");
        gh.backoff = Duration::ZERO;
        let (status, body) = gh
            .call("POST", &format!("{url}/x"), Some(json!({})))
            .unwrap();
        assert_eq!((status, body), (201, json!({"id": 1})));
        assert_eq!(*count.lock().unwrap(), 3);
    }

    #[test]
    fn gives_up_after_three_attempts() {
        let error =
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let (url, count) = serve(vec![error; 4]);
        let mut gh = GitHub::new(&url, "o/r", "t");
        gh.backoff = Duration::ZERO;
        assert_eq!(gh.call("GET", &format!("{url}/x"), None).unwrap().0, 503);
        assert_eq!(*count.lock().unwrap(), 3);
    }
}
