# Copilot + LM Studio: VS Code Config & Proxy

Generate GitHub Copilot "Custom OpenAI models" configuration for LM Studio, plus an optional HTTP proxy that patches older protocol quirks when needed.

---

## Quick Start (VS Code)

Most people just want Copilot to talk to LM Studio.

1. Build the binary:
   ```bash
   cargo build --release
   ```
2. Generate VS Code config and update `settings.json`:
   ```bash
   ./target/release/copilot-lmstudio-config generate-config --settings code-insiders
   ```
3. Review the diff, type `y` to apply, then restart VS Code.
4. In Copilot Chat, pick one of your LM Studio models from the model selector.

Python alternative:
```bash
uv run scripts/lm-studio-copilot-config.py --settings code-insiders
```

---

## VS Code Configuration (Main Feature)

This project’s primary job is to generate the `github.copilot.chat.customOAIModels` block so Copilot can talk to LM Studio.

### Using the Rust CLI (recommended)

The proxy binary includes a `generate-config` subcommand.

**Print configuration to stdout**:
```bash
./copilot-lmstudio-config generate-config
```

**Auto-detect VS Code settings** (recommended):
```bash
# For VS Code
./copilot-lmstudio-config generate-config --settings code

# For VS Code Insiders
./copilot-lmstudio-config generate-config --settings code-insiders
```

**Update VS Code settings with custom path** (macOS):
```bash
./copilot-lmstudio-config generate-config \
  --settings-path "~/Library/Application Support/Code/User/settings.json"
```

**Remote deployment** (proxy and LM Studio on remote host):
```bash
# Run this on your laptop, pointing to remote server
./copilot-lmstudio-config generate-config \
  --base-url http://gpu-server.local:3000/v1 \
  --lmstudio-url http://gpu-server.local:1234 \
  --settings-path "~/Library/Application Support/Code/User/settings.json"
```

**Options**:

```text
      --base-url <BASE_URL>            Base URL to write in VS Code config (where Copilot will connect) [default: http://localhost:3000/v1]
      --lmstudio-url <LMSTUDIO_URL>    LM Studio URL to fetch models from (defaults to base-url with port 1234)
      --settings <SETTINGS>            Auto-detect VS Code settings path (code or code-insiders) [possible values: code, code-insiders]
      --settings-path <SETTINGS_PATH>  Path to VS Code settings.json file (prints to stdout if not provided)
  -h, --help                           Print help
```

**Safety Features**:

- Shows a focused diff preview before applying changes
- Prompts for confirmation (`y/N`)
- Creates dated backups (`settings.YYMMDD-N.backup.json`) before modifying
- Supports JSONC format (comments, trailing commas) via `json5`

### Using the Python Script

The `scripts/lm-studio-copilot-config.py` script provides the same functionality for users who prefer Python, with the same options and behavior as the built-in `generate-config` subcommand.

#### Installation

The script uses PEP 723 inline dependencies and works best with `uv`:

```bash
# Install uv (if not already installed)
curl -LsSf https://astral.sh/uv/install.sh | sh

# Run the script (dependencies auto-installed)
uv run scripts/lm-studio-copilot-config.py --help
```

Alternatively, install dependencies manually:

```bash
pip install requests click json5
python scripts/lm-studio-copilot-config.py --help
```

#### Usage

**Print configuration to stdout** (copy/paste into VS Code settings):
```bash
uv run scripts/lm-studio-copilot-config.py
```

**Auto-detect VS Code settings** (recommended):
```bash
# For VS Code
uv run scripts/lm-studio-copilot-config.py --settings code

# For VS Code Insiders
uv run scripts/lm-studio-copilot-config.py --settings code-insiders
```

**Update VS Code settings with custom path**:
```bash
uv run scripts/lm-studio-copilot-config.py --settings-path "~/Library/Application Support/Code/User/settings.json"
```

**Remote deployment / custom LM Studio URL** (same semantics as the Rust command):
```bash
uv run scripts/lm-studio-copilot-config.py \
  --base-url http://gpu-server.local:3000/v1 \
  --lmstudio-url http://gpu-server.local:1234 \
  --settings-path "~/Library/Application Support/Code/User/settings.json"
```

The Python tool produces the same JSON structure and ordering as the Rust CLI, so you can freely switch between them.

### What Gets Written

Both tools update (or print) the `github.copilot.chat.customOAIModels` block. A single model entry looks like:

```json
{
  "github.copilot.chat.customOAIModels": {
    "qwen2.5-coder-32b-instruct": {
      "maxInputTokens": 32768,
      "maxOutputTokens": 32768,
      "name": "qwen2.5-coder-32b-instruct",
      "requiresAPIKey": false,
      "thinking": true,
      "toolCalling": true,
      "url": "http://localhost:3000/v1",
      "vision": false
    }
  }
}
```

- `url` should point to **where Copilot should send OpenAI-style requests**:
  - either directly to LM Studio (e.g. `http://localhost:1234/v1`), or
  - to the proxy (`http://localhost:3000/v1`) if you enable it.
- `toolCalling` and `vision` are auto-detected from LM Studio’s model capabilities.
- `maxInputTokens` / `maxOutputTokens` come from the model’s reported context length.

---

## Do You Need the Proxy?

With current LM Studio and Copilot, the raw HTTP protocol often works fine: you can point `url` directly at LM Studio’s `/v1` endpoint and skip the proxy.

The proxy exists mainly to smooth over specific incompatibilities that have shown up over time (usage fields, tool parameter schemas, streaming shapes, and headers). If everything works for you without it, you don’t need the proxy.

Recommended approach:

1. Start by generating config that points directly at LM Studio (e.g. `http://localhost:1234/v1`).
2. If you see errors from Copilot or LM Studio related to:
   - missing `input_tokens_details` / `output_tokens_details`,
   - invalid tool `parameters` schemas,
   - odd streaming chunks or header issues,
   then enable the proxy and point `url` at it instead.

---

## Proxy Server (Optional Compatibility Layer)

The proxy is a small Axum HTTP server that sits between Copilot and LM Studio and fixes known protocol mismatches on the fly.

### Build

```bash
# Debug build
cargo build

# Release build (optimized, single binary)
cargo build --release
```

The release binary will be at `target/release/copilot-lmstudio-config`.

### Run

```bash
# Development (default: localhost:3000 -> http://localhost:1234)
cargo run

# Or run the release binary directly (runs proxy by default)
./target/release/copilot-lmstudio-config

# Explicitly run proxy server with custom configuration
./target/release/copilot-lmstudio-config serve --port 8080 --lmstudio-url http://studio.local:1234

# Bind to all interfaces (accessible from network)
./target/release/copilot-lmstudio-config serve --bind-all

# Enable CORS for browser-based clients
./target/release/copilot-lmstudio-config serve --cors
```

**Proxy Server CLI Options** (`serve` subcommand):

- `-p, --port <PORT>` - Port to listen on (default: 3000)
- `-l, --lmstudio-url <URL>` - LM Studio base URL (default: http://localhost:1234)
- `-b, --bind-all` - Bind to `0.0.0.0` instead of `127.0.0.1`
- `-c, --cors` - Enable CORS (Cross-Origin Resource Sharing)

### Issues Fixed

- **Missing `input_tokens_details` in Responses API**
  - LM Studio doesn’t include `input_tokens_details.cached_tokens` in usage responses.
  - The proxy adds `input_tokens_details: { cached_tokens: 0 }` automatically.

- **Missing `output_tokens_details` in streaming/Responses usage**
  - The proxy injects `output_tokens_details: { reasoning_tokens: 0 }` where required by Copilot.

- **Tool parameters missing `type: "object"`**
  - Copilot sometimes sends tools with `parameters: {}` instead of a valid JSON Schema object.
  - The proxy normalizes these to `parameters: { type: "object", properties: {} }`, supporting both OpenAI function-calling and direct parameter formats.

- **Header / encoding mismatches**
  - Strips or adjusts hop-by-hop headers after reqwest’s automatic decompression so Copilot’s client doesn’t get confused.

---

## Logging

Use `RUST_LOG` to control verbosity:

```bash
# More verbose
RUST_LOG=debug cargo run

# Less verbose
RUST_LOG=warn cargo run
```

---

## Development

- Rust code: `src/main.rs`
- Python helper: `scripts/lm-studio-copilot-config.py`

Useful commands:

```bash
cargo test
cargo run -- generate-config --help
uv run scripts/lm-studio-copilot-config.py --help
```

---

## License

MIT (see `LICENSE` if present).
