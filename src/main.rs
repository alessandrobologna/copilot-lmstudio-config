use axum::{
    Router,
    body::{Body, Bytes},
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
};
use clap::Parser;
use futures::StreamExt;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serde::Serialize;
use std::sync::OnceLock;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use chrono::Datelike;

static CONFIG: OnceLock<ServeConfig> = OnceLock::new();
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(name = "copilot-lmstudio-config")]
#[command(about = "Configure GitHub Copilot for LM Studio (plus optional compatibility proxy)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Run the proxy server (default)
    Serve(ServeConfig),
    /// Generate VS Code configuration from LM Studio models
    GenerateConfig(GenerateConfigArgs),
}

#[derive(Parser, Debug, Clone)]
struct ServeConfig {
    /// Port to listen on
    #[arg(short, long, default_value_t = 3000)]
    port: u16,

    /// LMStudio base URL
    #[arg(short, long, default_value = "http://localhost:1234")]
    lmstudio_url: String,

    /// Bind to all interfaces (0.0.0.0) instead of localhost only
    #[arg(short, long, default_value_t = false)]
    bind_all: bool,

    /// Enable CORS (Cross-Origin Resource Sharing)
    #[arg(short, long, default_value_t = false)]
    cors: bool,
}

#[derive(clap::ValueEnum, Debug, Clone)]
enum VsCodeEditor {
    Code,
    CodeInsiders,
}

#[derive(Parser, Debug)]
struct GenerateConfigArgs {
    /// Base URL to write in VS Code config (where Copilot will connect)
    #[arg(long, default_value = "http://localhost:3000/v1")]
    base_url: String,

    /// LM Studio URL to fetch models from (defaults to base-url with port 1234)
    #[arg(long)]
    lmstudio_url: Option<String>,

    /// Auto-detect VS Code settings path (code or code-insiders)
    #[arg(long, value_enum)]
    settings: Option<VsCodeEditor>,

    /// Path to VS Code settings.json file (prints to stdout if not provided)
    #[arg(long, conflicts_with = "settings")]
    settings_path: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Serve(config)) => serve(config).await,
        Some(Command::GenerateConfig(args)) => {
            if let Err(e) = generate_config(args).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // Default to serve if no subcommand provided
            serve(ServeConfig::parse()).await
        }
    }
}

fn get_vscode_settings_path(
    editor: &VsCodeEditor,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::env;
    use std::path::PathBuf;

    let home = dirs::home_dir().ok_or("Could not determine home directory")?;

    let path = match (std::env::consts::OS, editor) {
        ("macos", VsCodeEditor::Code) => {
            home.join("Library/Application Support/Code/User/settings.json")
        }
        ("macos", VsCodeEditor::CodeInsiders) => {
            home.join("Library/Application Support/Code - Insiders/User/settings.json")
        }
        ("windows", VsCodeEditor::Code) => {
            let appdata = env::var("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join("AppData/Roaming"));
            appdata.join("Code/User/settings.json")
        }
        ("windows", VsCodeEditor::CodeInsiders) => {
            let appdata = env::var("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join("AppData/Roaming"));
            appdata.join("Code - Insiders/User/settings.json")
        }
        ("linux", VsCodeEditor::Code) => home.join(".config/Code/User/settings.json"),
        ("linux", VsCodeEditor::CodeInsiders) => {
            home.join(".config/Code - Insiders/User/settings.json")
        }
        (os, _) => return Err(format!("Unsupported OS: {}", os).into()),
    };

    Ok(path)
}

enum DiffDecision {
    Unchanged,
    Apply,
    Cancel,
}

fn show_diff_and_confirm(
    old_content: &str,
    new_content: &str,
    file_path: &str,
) -> Result<DiffDecision, Box<dyn std::error::Error>> {
    use similar::{ChangeTag, TextDiff};
    use std::io::{self, Write};

    let diff = TextDiff::from_lines(old_content, new_content);

    println!("\nDiff preview for: {}\n", file_path);

    let mut has_changes = false;
    for change in diff.iter_all_changes() {
        if change.tag() != ChangeTag::Equal {
            has_changes = true;
            break;
        }
    }

    if !has_changes {
        println!("No changes detected.");
        return Ok(DiffDecision::Unchanged);
    }

    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Equal {
            continue;
        }

        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };

        let color = match change.tag() {
            ChangeTag::Delete => "\x1b[31m", // Red
            ChangeTag::Insert => "\x1b[32m", // Green
            ChangeTag::Equal => "",
        };
        let reset = if color.is_empty() { "" } else { "\x1b[0m" };

        print!("{}{}{}{}", color, sign, change, reset);
    }

    println!();

    print!("\nApply these changes? [y/N]: ");
    io::stdout().flush()?;

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;

    let decision = match response.trim().to_lowercase().as_str() {
        "y" | "yes" => DiffDecision::Apply,
        _ => DiffDecision::Cancel,
    };

    Ok(decision)
}

// Structs for LM Studio API
#[derive(serde::Deserialize, Debug)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(serde::Deserialize, Debug)]
struct ModelInfo {
    id: String,
    #[serde(rename = "type")]
    model_type: Option<String>,
    capabilities: Option<Vec<String>>,
    max_context_length: Option<u32>,
}

// Structs for VS Code config
#[derive(serde::Serialize, Debug)]
struct CopilotConfig {
    name: String,
    url: String,
    #[serde(rename = "toolCalling")]
    tool_calling: bool,
    vision: bool,
    thinking: bool,
    #[serde(rename = "maxInputTokens")]
    max_input_tokens: u32,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    #[serde(rename = "requiresAPIKey")]
    requires_api_key: bool,
}

type ModelsMap = std::collections::BTreeMap<String, CopilotConfig>;

async fn generate_config(args: GenerateConfigArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Determine the settings path
    let final_settings_path = if let Some(ref editor) = args.settings {
        let path = get_vscode_settings_path(editor)?;
        println!("Using settings file: {}", path.display());
        Some(path.to_string_lossy().to_string())
    } else {
        args.settings_path
    };

    // Derive LM Studio URL from base-url if not provided
    let lmstudio_url = args.lmstudio_url.unwrap_or_else(|| {
        // Replace port with 1234 for LM Studio default
        let base = args.base_url.trim_end_matches("/v1").trim_end_matches('/');
        if let Some(idx) = base.rfind(':') {
            let host_part = &base[..idx];
            format!("{}:1234", host_part)
        } else {
            "http://localhost:1234".to_string()
        }
    });

    // Fetch models from LM Studio
    let models_url = format!("{}/api/v0/models", lmstudio_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let response = match client.get(&models_url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            if e.is_connect() {
                eprintln!("\nError: Could not connect to LM Studio at {}", lmstudio_url);
                eprintln!("\nPlease ensure:");
                eprintln!("  1. LM Studio is running");
                eprintln!("  2. Local server is started in LM Studio");
                eprintln!("  3. Server is listening on the correct port");
                eprintln!("\nIf LM Studio is running on a different port, use:");
                eprintln!("  --lmstudio-url http://localhost:PORT");
                std::process::exit(1);
            } else {
                return Err(format!("Error sending request to {}: {}", models_url, e).into());
            }
        }
    };

    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch models from {}: {}",
            models_url,
            response.status()
        )
        .into());
    }

    let models_response: ModelsResponse = response.json().await?;

    // Generate config for each model (sorted by model id for stable ordering)
    let mut config_map: ModelsMap = ModelsMap::new();
    for model in models_response.data {
        // Only include LLM and VLM models
        if let Some(ref model_type) = model.model_type
            && model_type != "llm"
            && model_type != "vlm"
        {
            continue;
        }

        let capabilities = model.capabilities.as_ref();
        let max_context = model.max_context_length.unwrap_or(8192);

        let copilot_config = CopilotConfig {
            name: model.id.clone(),
            url: args.base_url.clone(),
            tool_calling: capabilities
                .map(|caps| caps.contains(&"tool_use".to_string()))
                .unwrap_or(false),
            vision: model.model_type.as_ref()
                .map(|t| t == "vlm")
                .unwrap_or(false),
            thinking: true,
            max_input_tokens: max_context,
            max_output_tokens: max_context,
            requires_api_key: false,
        };

        config_map.insert(model.id, copilot_config);
    }

    // Output configuration
    if let Some(settings_path) = final_settings_path {
        update_settings_file(&settings_path, &config_map)?;
    } else {
        let output = json!({
            "github.copilot.chat.customOAIModels": config_map
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

fn detect_indentation(content: &str) -> String {
    // Try to detect indentation from the first indented line
    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            let indent = line.chars().take_while(|c| c.is_whitespace()).collect::<String>();
            if !indent.is_empty() {
                return indent;
            }
        }
    }
    // Default to 2 spaces if we can't detect
    "  ".to_string()
}

fn serialize_with_indent(value: &Value, indent: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser)?;
    Ok(String::from_utf8(buf)?)
}
fn render_models_object(
    config: &ModelsMap,
    indent_unit: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let value = serde_json::to_value(config)?;
    serialize_with_indent(&value, indent_unit)
}

fn try_update_custom_oai_models_in_text(
    old_content: &str,
    models_object_src: &str,
) -> Option<String> {
    let key = "github.copilot.chat.customOAIModels";
    let bytes = old_content.as_bytes();
    let key_bytes = key.as_bytes();
    let len = bytes.len();

    let mut i = 0usize;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut string_delim = b'"';
    let mut escaped = false;

    while i < len {
        let b = bytes[i];

        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == string_delim {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if b == b'/' && i + 1 < len {
            if bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
        }

        if b == b'"' || b == b'\'' {
            let delim = b;
            let start = i;
            let after_start = i + 1;

            if after_start + key_bytes.len() < len
                && &bytes[after_start..after_start + key_bytes.len()] == key_bytes
                && after_start + key_bytes.len() < len
                && bytes[after_start + key_bytes.len()] == delim
            {
                let mut k = after_start + key_bytes.len() + 1;
                while k < len
                    && (bytes[k] == b' ' || bytes[k] == b'\t' || bytes[k] == b'\n' || bytes[k] == b'\r')
                {
                    k += 1;
                }
                if k >= len || bytes[k] != b':' {
                    // Not actually a key, treat as string
                } else {
                    let line_start = old_content[..start]
                        .rfind('\n')
                        .map(|idx| idx + 1)
                        .unwrap_or(0);
                    let property_indent: String = old_content[line_start..start]
                        .chars()
                        .take_while(|c| c.is_whitespace())
                        .collect();

                    k += 1; // skip ':'
                    while k < len && (bytes[k] == b' ' || bytes[k] == b'\t') {
                        k += 1;
                    }
                    if k >= len || bytes[k] != b'{' {
                        return None;
                    }
                    let value_start = k;

                    let mut depth = 0i32;
                    let mut m = value_start;
                    let mut in_line_comment2 = false;
                    let mut in_block_comment2 = false;
                    let mut in_string2 = false;
                    let mut string_delim2 = b'"';
                    let mut escaped2 = false;

                    while m < len {
                        let cb = bytes[m];

                        if in_line_comment2 {
                            if cb == b'\n' {
                                in_line_comment2 = false;
                            }
                            m += 1;
                            continue;
                        }
                        if in_block_comment2 {
                            if cb == b'*' && m + 1 < len && bytes[m + 1] == b'/' {
                                in_block_comment2 = false;
                                m += 2;
                            } else {
                                m += 1;
                            }
                            continue;
                        }
                        if in_string2 {
                            if escaped2 {
                                escaped2 = false;
                            } else if cb == b'\\' {
                                escaped2 = true;
                            } else if cb == string_delim2 {
                                in_string2 = false;
                            }
                            m += 1;
                            continue;
                        }

                        if cb == b'/' && m + 1 < len {
                            if bytes[m + 1] == b'/' {
                                in_line_comment2 = true;
                                m += 2;
                                continue;
                            }
                            if bytes[m + 1] == b'*' {
                                in_block_comment2 = true;
                                m += 2;
                                continue;
                            }
                        }

                        if cb == b'"' || cb == b'\'' {
                            in_string2 = true;
                            string_delim2 = cb;
                            escaped2 = false;
                            m += 1;
                            continue;
                        }

                        if cb == b'{' {
                            depth += 1;
                        } else if cb == b'}' {
                            depth -= 1;
                            if depth == 0 {
                                let value_end = m + 1;

                                let mut replacement = String::new();
                                if models_object_src.contains('\n') {
                                    let mut first = true;
                                    for chunk in models_object_src.split_inclusive('\n') {
                                        if first {
                                            replacement.push_str(chunk);
                                            first = false;
                                        } else {
                                            replacement.push_str(&property_indent);
                                            replacement.push_str(chunk);
                                        }
                                    }
                                } else {
                                    replacement.push_str(models_object_src);
                                }

                                let mut new_content =
                                    String::with_capacity(old_content.len() + models_object_src.len());
                                new_content.push_str(&old_content[..value_start]);
                                new_content.push_str(&replacement);
                                new_content.push_str(&old_content[value_end..]);

                                if json5::from_str::<Value>(&new_content).is_ok() {
                                    return Some(new_content);
                                } else {
                                    return None;
                                }
                            }
                        }

                        m += 1;
                    }

                    return None;
                }
            }

            in_string = true;
            string_delim = delim;
            escaped = false;
            i += 1;
            continue;
        }

        i += 1;
    }

    None
}

fn update_settings_file(
    settings_path: &str,
    config: &ModelsMap,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use std::path::PathBuf;

    let expanded_path = shellexpand::tilde(settings_path);
    let settings_file = PathBuf::from(expanded_path.as_ref());

    let old_content = if settings_file.exists() {
        fs::read_to_string(&settings_file)?
    } else {
        String::new()
    };

    let indent_unit = detect_indentation(&old_content);
    let models_object_src = render_models_object(config, &indent_unit)?;

    let new_content = if !old_content.is_empty() {
        if let Some(updated) =
            try_update_custom_oai_models_in_text(&old_content, &models_object_src)
        {
            updated
        } else {
            let mut settings: Value = json5::from_str(&old_content).unwrap_or_else(|e| {
                eprintln!(
                    "Warning: Could not parse existing settings ({}), creating new structure...",
                    e
                );
                json!({})
            });
            settings["github.copilot.chat.customOAIModels"] = serde_json::to_value(config)?;
            serialize_with_indent(&settings, &indent_unit)?
        }
    } else {
        let mut settings = json!({});
        settings["github.copilot.chat.customOAIModels"] = serde_json::to_value(config)?;
        serialize_with_indent(&settings, &indent_unit)?
    };

    // Show diff and ask for confirmation (if there are changes)
    match show_diff_and_confirm(&old_content, &new_content, settings_path)? {
        DiffDecision::Unchanged => {
            // Nothing to do, leave file and backup untouched.
            return Ok(());
        }
        DiffDecision::Cancel => {
            println!("Operation cancelled by user");
            std::process::exit(0);
        }
        DiffDecision::Apply => {
            // proceed below
        }
    }

    // Create dated backup before modifying, e.g. settings.250924-0.backup.json
    if settings_file.exists() {
        let now = chrono::Local::now();
        let y = now.year() % 100;
        let m = now.month();
        let d = now.day();
        let date_tag = format!("{:02}{:02}{:02}", y, m, d);

        let stem = settings_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("settings");

        let mut index = 0u32;
        let backup_path = loop {
            let filename = format!("{stem}.{date_tag}-{index}.backup.json");
            let candidate = settings_file.with_file_name(&filename);
            if !candidate.exists() {
                break candidate;
            }
            index += 1;
        };

        fs::copy(&settings_file, &backup_path)?;
        println!("Created backup at {}", backup_path.display());
    }

    // Write back to file (as regular JSON with proper formatting)
    fs::write(&settings_file, new_content)?;

    println!(
        "Updated {} with {} models",
        settings_file.display(),
        config.len()
    );

    Ok(())
}

async fn serve(config: ServeConfig) {
    CONFIG.set(config.clone()).expect("Failed to set config");

    // Initialize HTTP client (reused for all requests for connection pooling)
    let client = reqwest::Client::builder()
        .http1_only() // LMStudio might not support HTTP/2
        .build()
        .expect("Failed to create HTTP client");
    HTTP_CLIENT.set(client).expect("Failed to set HTTP client");

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "copilot_lmstudio_config=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let bind_addr = if config.bind_all {
        format!("0.0.0.0:{}", config.port)
    } else {
        format!("127.0.0.1:{}", config.port)
    };

    info!("Copilot-LMStudio Proxy starting");
    info!("  Listening: http://{}", bind_addr);
    info!("  Upstream: {}", config.lmstudio_url);
    if config.cors {
        info!("  CORS: enabled");
    }

    let mut app = Router::new().fallback(any(proxy_handler));

    // Add CORS layer if enabled
    if config.cors {
        use tower_http::cors::{Any, CorsLayer};
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
        app = app.layer(cors);
    }

    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();

    info!("Proxy ready!");
    axum::serve(listener, app).await.unwrap();
}

async fn proxy_handler(req: Request) -> Result<Response, StatusCode> {
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let path = uri.path();
    let query = uri.query().unwrap_or("");

    info!(
        "{} {} {}",
        method,
        path,
        if query.is_empty() {
            ""
        } else {
            &format!("?{}", query)
        }
    );

    // Read the original body
    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    let config = CONFIG.get().expect("Config not initialized");

    // Try to parse and fix the body if it's JSON
    let fixed_body_bytes = if !body_bytes.is_empty() && is_json_request(&parts.headers) {
        match fix_request_body(&body_bytes) {
            Ok(fixed) => fixed,
            Err(e) => {
                warn!("Could not fix request body: {}", e);
                body_bytes
            }
        }
    } else {
        body_bytes
    };

    // Build the upstream URL
    let lmstudio_base = config.lmstudio_url.trim_end_matches('/');
    let upstream_url = format!("{}{}", lmstudio_base, path);
    let upstream_url_with_query = if query.is_empty() {
        upstream_url.clone()
    } else {
        format!("{}?{}", upstream_url, query)
    };

    // Create upstream request using the shared client
    let client = HTTP_CLIENT.get().expect("HTTP client not initialized");
    let mut upstream_req = client.request(method.clone(), &upstream_url_with_query);

    // Copy headers (except Host and problematic headers)
    for (name, value) in parts.headers.iter() {
        let name_str = name.as_str();
        // Skip host and headers that might cause issues. Reqwest recalculates
        // connection management, compression, and body length on our behalf.
        if name_str == "host"
            || name_str.starts_with("sec-")
            || name_str == "connection"
            || name_str == "accept-encoding"
            || name_str == "content-length"
        {
            continue;
        }

        upstream_req = upstream_req.header(name, value);
    }

    // Add body
    upstream_req = upstream_req.body(fixed_body_bytes);

    // Send request to LMStudio
    let upstream_response = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            error!("Failed to proxy request: {}", e);
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    let status = upstream_response.status();
    let mut headers = upstream_response.headers().clone();

    if !status.is_success() {
        warn!("Response: {}", status);
    }

    // Check if this is a streaming response
    let is_streaming = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);

    // Strip hop-by-hop and encoding headers after reqwest's automatic decompression
    sanitize_response_headers(&mut headers);

    if is_streaming {
        // Handle streaming response
        let stream = upstream_response.bytes_stream();
        let fixed_stream = stream.map(move |chunk_result| match chunk_result {
            Ok(chunk) => match fix_streaming_chunk(&chunk) {
                Ok(fixed) => Ok(fixed),
                Err(_) => Ok(chunk),
            },
            Err(e) => Err(std::io::Error::other(e)),
        });

        let body = Body::from_stream(fixed_stream);
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;

        Ok(response)
    } else {
        // Handle non-streaming response
        let body_bytes = match upstream_response.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to read response body: {}", e);
                return Err(StatusCode::BAD_GATEWAY);
            }
        };

        let fixed_body_bytes = if is_json_response(&headers) {
            match fix_response_body(&body_bytes) {
                Ok(fixed) => fixed,
                Err(_) => body_bytes,
            }
        } else {
            body_bytes
        };

        let mut response = Response::new(Body::from(fixed_body_bytes));
        *response.status_mut() = status;
        *response.headers_mut() = headers;

        Ok(response)
    }
}

fn is_json_request(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false)
}

fn is_json_response(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false)
}

fn fix_request_body(body: &Bytes) -> Result<Bytes, Box<dyn std::error::Error>> {
    let mut json: Value = serde_json::from_slice(body)?;

    // Fix tools array (Issue #2)
    if let Some(tools) = json.get_mut("tools").and_then(|t| t.as_array_mut()) {
        let mut fixed_count = 0;
        for tool in tools.iter_mut() {
            // Handle both formats:
            // 1. OpenAI function calling: tool.function.parameters
            // 2. Direct format: tool.parameters
            let parameters_ref = if let Some(function) = tool.get_mut("function") {
                function.get_mut("parameters")
            } else {
                tool.get_mut("parameters")
            };

            if let Some(parameters) = parameters_ref {
                // If parameters is an object without a type field, or an empty object
                if parameters.is_object() {
                    let params_obj = parameters.as_object_mut().unwrap();
                    if !params_obj.contains_key("type") {
                        params_obj.insert("type".to_string(), json!("object"));
                        if !params_obj.contains_key("properties") {
                            params_obj.insert("properties".to_string(), json!({}));
                        }
                        fixed_count += 1;
                    }
                }
            }
        }
        if fixed_count > 0 {
            info!("Fixed {} tool parameter schema(s)", fixed_count);
        }
    }

    Ok(Bytes::from(serde_json::to_vec(&json)?))
}

fn fix_response_body(body: &Bytes) -> Result<Bytes, Box<dyn std::error::Error>> {
    let mut json: Value = serde_json::from_slice(body)?;

    // Fix usage details (Issue #1)
    if let Some(usage) = json.get_mut("usage").and_then(|u| u.as_object_mut()) {
        let mut fixed = false;

        if !usage.contains_key("input_tokens_details") {
            usage.insert(
                "input_tokens_details".to_string(),
                json!({"cached_tokens": 0}),
            );
            fixed = true;
        }

        if !usage.contains_key("output_tokens_details") {
            usage.insert(
                "output_tokens_details".to_string(),
                json!({"reasoning_tokens": 0}),
            );
            fixed = true;
        }

        if fixed {
            info!("Fixed usage details in response");
        }
    }

    Ok(Bytes::from(serde_json::to_vec(&json)?))
}

fn fix_streaming_chunk(chunk: &Bytes) -> Result<Bytes, Box<dyn std::error::Error>> {
    let chunk_str = std::str::from_utf8(chunk)?;

    // SSE format: "data: {...}\n\n"
    if !chunk_str.starts_with("data: ") {
        return Ok(chunk.clone());
    }

    // Extract the JSON part
    let data_line = chunk_str.trim_start_matches("data: ").trim();

    // Skip [DONE] marker
    if data_line == "[DONE]" {
        return Ok(chunk.clone());
    }

    // Try to parse and fix the JSON
    let mut json: Value = match serde_json::from_str(data_line) {
        Ok(j) => j,
        Err(_) => return Ok(chunk.clone()),
    };

    let mut fixed = false;

    // Fix for Responses API streaming
    if let Some(response) = json.get_mut("response")
        && let Some(usage) = response.get_mut("usage").and_then(|u| u.as_object_mut())
    {
        if !usage.contains_key("input_tokens_details") {
            usage.insert(
                "input_tokens_details".to_string(),
                json!({"cached_tokens": 0}),
            );
            fixed = true;
        }
        if !usage.contains_key("output_tokens_details") {
            usage.insert(
                "output_tokens_details".to_string(),
                json!({"reasoning_tokens": 0}),
            );
            fixed = true;
        }
    }

    if fixed {
        let fixed_json_str = serde_json::to_string(&json)?;
        let fixed_chunk = format!("data: {}\n\n", fixed_json_str);
        Ok(Bytes::from(fixed_chunk))
    } else {
        Ok(chunk.clone())
    }
}

fn sanitize_response_headers(headers: &mut HeaderMap) {
    // These headers no longer reflect reality after reqwest decompressed the payload.
    headers.remove("content-encoding");
    headers.remove("transfer-encoding");
    headers.remove("content-length");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use bytes::Bytes;

    #[test]
    fn fixes_missing_tool_parameter_schema() {
        let input = json!({
            "tools": [
                { "function": { "parameters": {} } },
                { "parameters": {} },
                {
                    "function": {
                        "parameters": {
                            "type": "object",
                            "properties": { "foo": { "type": "string" } }
                        }
                    }
                }
            ]
        });

        let bytes = Bytes::from(serde_json::to_vec(&input).unwrap());
        let fixed = fix_request_body(&bytes).expect("request body fix should succeed");
        let fixed_json: Value = serde_json::from_slice(&fixed).unwrap();
        let tools = fixed_json["tools"]
            .as_array()
            .expect("tools should remain an array");

        let first_params = tools[0]["function"]["parameters"].as_object().unwrap();
        assert_eq!(first_params["type"], "object");
        assert!(first_params["properties"].as_object().unwrap().is_empty());

        let second_params = tools[1]["parameters"].as_object().unwrap();
        assert_eq!(second_params["type"], "object");
        assert!(second_params["properties"].as_object().unwrap().is_empty());

        let third_params = tools[2]["function"]["parameters"].as_object().unwrap();
        assert_eq!(third_params["type"], "object");
        assert_eq!(
            third_params["properties"].as_object().unwrap()["foo"],
            json!({ "type": "string" })
        );
    }

    #[test]
    fn adds_missing_usage_details() {
        let input = json!({
            "usage": {}
        });

        let bytes = Bytes::from(serde_json::to_vec(&input).unwrap());
        let fixed = fix_response_body(&bytes).expect("response body fix should succeed");
        let fixed_json: Value = serde_json::from_slice(&fixed).unwrap();
        let usage = fixed_json["usage"].as_object().unwrap();

        assert_eq!(usage["input_tokens_details"], json!({ "cached_tokens": 0 }));
        assert_eq!(
            usage["output_tokens_details"],
            json!({ "reasoning_tokens": 0 })
        );
    }

    #[test]
    fn fixes_streaming_usage_chunks() {
        let chunk = Bytes::from("data: {\"response\":{\"usage\":{}}}\n\n");
        let fixed = fix_streaming_chunk(&chunk).expect("stream chunk fix should succeed");
        assert_ne!(fixed, chunk);

        let fixed_str = std::str::from_utf8(&fixed).unwrap();
        assert!(fixed_str.contains("input_tokens_details"));
        assert!(fixed_str.contains("output_tokens_details"));
    }

    #[test]
    fn leaves_done_streaming_marker_untouched() {
        let chunk = Bytes::from("data: [DONE]\n\n");
        let fixed = fix_streaming_chunk(&chunk).expect("[DONE] chunk fix should succeed");
        assert_eq!(fixed, chunk);
    }

    #[test]
    fn sanitizes_decompressed_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", HeaderValue::from_static("gzip"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        headers.insert("content-length", HeaderValue::from_static("42"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        sanitize_response_headers(&mut headers);

        assert!(headers.get("content-encoding").is_none());
        assert!(headers.get("transfer-encoding").is_none());
        assert!(headers.get("content-length").is_none());
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn test_model_info_deserialization() {
        let json = r#"{
            "id": "test-model",
            "type": "llm",
            "capabilities": ["tool_use", "vision"],
            "max_context_length": 32768
        }"#;

        let model: ModelInfo = serde_json::from_str(json).unwrap();
        assert_eq!(model.id, "test-model");
        assert_eq!(model.model_type, Some("llm".to_string()));
        assert_eq!(
            model.capabilities,
            Some(vec!["tool_use".to_string(), "vision".to_string()])
        );
        assert_eq!(model.max_context_length, Some(32768));
    }

    #[test]
    fn test_copilot_config_serialization() {
        let config = CopilotConfig {
            name: "test-model".to_string(),
            url: "http://localhost:3000/v1".to_string(),
            tool_calling: true,
            vision: false,
            thinking: true,
            max_input_tokens: 32768,
            max_output_tokens: 32768,
            requires_api_key: false,
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["name"], "test-model");
        assert_eq!(json["toolCalling"], true);
        assert_eq!(json["vision"], false);
        assert_eq!(json["maxInputTokens"], 32768);
    }

    #[test]
    fn test_path_expansion() {
        use shellexpand::tilde;
        let path = "~/test/path";
        let expanded = tilde(path);
        assert!(!expanded.starts_with('~'));
    }
}
