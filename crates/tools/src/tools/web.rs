#[derive(Clone, Debug)]
struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "web_fetch",
            "Fetch remote documents and APIs.",
            ToolKind::Network,
            true,
            true,
        )
    }

    async fn invoke(&self, input: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let url = input_string(&input, "url")?;
        let method = Method::from_bytes(input_string_or(&input, "method", "GET").as_bytes())?;
        let headers = string_map_field(&input, "headers")?;
        let body = input.get("body").and_then(Value::as_str).map(str::to_owned);
        let client = reqwest::Client::new();
        let mut request = client.request(method.clone(), &url);
        for (key, value) in &headers {
            request = request.header(key, value);
        }
        if let Some(body) = body {
            request = request.body(body);
        }

        let response = request.send().await?;
        let status = response.status();
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await?;

        Ok(ToolOutput {
            content: text,
            is_error: !status.is_success(),
            metadata: json!({
                "url": final_url,
                "status": status.as_u16(),
                "method": method.as_str(),
                "content_type": content_type,
                "header_count": headers.len(),
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct WebSearchTool;

fn collect_search_results(value: &Value, results: &mut Vec<Value>) {
    if let Some(url) = value.get("FirstURL").and_then(Value::as_str) {
        results.push(json!({
            "title": value.get("Text").and_then(Value::as_str).unwrap_or(url),
            "url": url,
            "snippet": value.get("Text").and_then(Value::as_str).unwrap_or_default(),
        }));
    }
    if let Some(items) = value.get("RelatedTopics").and_then(Value::as_array) {
        for item in items {
            collect_search_results(item, results);
        }
    }
    if let Some(items) = value.as_array() {
        for item in items {
            collect_search_results(item, results);
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "web_search",
            "Search the web for context and sources.",
            ToolKind::Network,
            true,
            true,
        )
    }

    async fn invoke(&self, input: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let query = input_string(&input, "query")?;
        let limit = input_u64_or(&input, "limit", 5) as usize;
        let base_url = input_string_or(&input, "base_url", "https://api.duckduckgo.com/");
        let response = reqwest::Client::new()
            .get(&base_url)
            .query(&[
                ("q", query.as_str()),
                ("format", "json"),
                ("no_html", "1"),
                ("skip_disambig", "1"),
            ])
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        let mut results = Vec::new();
        if let Some(text) = value.get("AbstractText").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                results.push(json!({
                    "title": value.get("Heading").and_then(Value::as_str).unwrap_or("abstract"),
                    "url": value.get("AbstractURL").and_then(Value::as_str).unwrap_or_default(),
                    "snippet": text,
                }));
            }
        }
        collect_search_results(&value, &mut results);
        results.truncate(limit);
        let content = results
            .iter()
            .map(|entry| {
                format!(
                    "{}\n{}\n{}",
                    entry["title"].as_str().unwrap_or_default(),
                    entry["url"].as_str().unwrap_or_default(),
                    entry["snippet"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(ToolOutput {
            content,
            is_error: !status.is_success(),
            metadata: json!({ "query": query, "results": results }),
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct BrowserSessionState {
    id: String,
    current_url: Option<String>,
    history: Vec<String>,
    title: Option<String>,
    page_html: Option<String>,
}

#[derive(Clone, Debug)]
struct WebBrowserTool;

fn browser_session_dir(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("browser")
}

fn browser_session_path(cwd: &Path, session_id: &str) -> PathBuf {
    browser_session_dir(cwd).join(format!("{session_id}.json"))
}

fn load_browser_session(cwd: &Path, session_id: &str) -> Result<BrowserSessionState> {
    let path = browser_session_path(cwd, session_id);
    if !path.exists() {
        return Ok(BrowserSessionState {
            id: session_id.to_owned(),
            ..BrowserSessionState::default()
        });
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)
        .with_context(|| format!("failed to decode {}", path.display()))?)
}

fn save_browser_session(cwd: &Path, state: &BrowserSessionState) -> Result<PathBuf> {
    let path = browser_session_path(cwd, &state.id);
    ensure_parent_dir(&path)?;
    fs::write(&path, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

#[async_trait]
impl Tool for WebBrowserTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "web_browser",
            "Drive a browser-backed research session.",
            ToolKind::Network,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let action = input_string_or(&input, "action", "open");
        let session_id = optional_string(&input, "session_id")
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut state = load_browser_session(&context.cwd, &session_id)?;
        match action.as_str() {
            "open" | "navigate" => {
                let url = input_string(&input, "url")?;
                let response = reqwest::Client::new().get(&url).send().await?;
                let status = response.status();
                let html = response.text().await?;
                let title = html
                    .split("<title>")
                    .nth(1)
                    .and_then(|rest| rest.split("</title>").next())
                    .map(str::trim)
                    .map(str::to_owned);
                state.current_url = Some(url.clone());
                state.history.push(url.clone());
                state.title = title;
                state.page_html = Some(html.clone());
                let path = save_browser_session(&context.cwd, &state)?;
                Ok(ToolOutput {
                    content: strip_html_tags(&html),
                    is_error: !status.is_success(),
                    metadata: json!({ "session_id": session_id, "path": path, "url": url, "status": status.as_u16(), "title": state.title }),
                })
            }
            "extract_text" => {
                let html = state
                    .page_html
                    .clone()
                    .ok_or_else(|| anyhow!("browser session has no active page"))?;
                Ok(ToolOutput {
                    content: strip_html_tags(&html),
                    is_error: false,
                    metadata: json!({ "session_id": session_id, "url": state.current_url }),
                })
            }
            "history" => Ok(ToolOutput {
                content: state.history.join("\n"),
                is_error: false,
                metadata: json!({ "session_id": session_id, "history": state.history }),
            }),
            "get" => Ok(ToolOutput {
                content: state.page_html.unwrap_or_default(),
                is_error: false,
                metadata: json!({ "session_id": session_id, "url": state.current_url, "title": state.title }),
            }),
            "reset" => {
                let path = browser_session_path(&context.cwd, &session_id);
                if path.exists() {
                    fs::remove_file(&path)
                        .with_context(|| format!("failed to remove {}", path.display()))?;
                }
                Ok(ToolOutput {
                    content: format!("cleared browser session {session_id}"),
                    is_error: false,
                    metadata: json!({ "session_id": session_id, "path": path }),
                })
            }
            other => bail!("unsupported web_browser action: {other}"),
        }
    }
}
