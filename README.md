# Copilot + LM Studio: VS Code Config & Proxy

Generate GitHub Copilot "Custom OpenAI models" configuration for LM Studio, plus an optional HTTP proxy that patches older protocol quirks when needed.

---

## Quick Start (VS Code)

Most users simply want Copilot to communicate with LM Studio to run open weight models, but Copilot lacks a built-in way to enumerate the available models and requires manual configuration for each one.
Both the python package and rust package allow you to query the `v0` LM Studio API and to update the VS Code configuration, adding an entry `github.copilot.chat.customOAIModels {}` to it.

With the rust package, clone the repo, then build and run:
  ```bash
  # Config generation is the default - these are equivalent:
  cargo run -- --help
  cargo run -- generate-config --help

  # To run the proxy server:
  cargo run -- serve --help
  ```

With the python package, you can use `uvx` with no install:
```bash
uvx git+https://github.com/alessandrobologna/copilot-lmstudio-config --help
```

---
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
- `toolCalling` is auto-detected from the model's capabilities array.
- `vision` is auto-detected from the model type (true for VLM models).
- `maxInputTokens` / `maxOutputTokens` come from the model's reported context length.

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

### Build & Run

```bash
# Build
cargo build --release

# Run the proxy server
cargo run -- serve
# Or with custom options:
cargo run -- serve --port 3000 --lmstudio-url http://localhost:1234
```

The release binary will be at `target/release/copilot-lmstudio-config`.

**Note:** Config generation is now the default command. Use `serve` subcommand explicitly to run the proxy.

### Usage Examples

```bash
# Run proxy server (default: localhost:3000 -> http://localhost:1234)
cargo run -- serve

# Or run the release binary
./target/release/copilot-lmstudio-config serve

# Proxy server with custom configuration
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

Use `RUST_LOG` to control verbosity when running the proxy:

```bash
# More verbose
RUST_LOG=debug cargo run -- serve

# Less verbose
RUST_LOG=warn cargo run -- serve
```

---

## Development

- Rust code: `src/main.rs`
- Python package: `copilot_lmstudio_config/cli.py`

Useful commands:

```bash
# Run tests
cargo test

# Generate config (Rust)
cargo run -- --help

# Generate config (Python)
uv run copilot-lmstudio-config --help

# Run proxy server
cargo run -- serve
```

---

## License

MIT
