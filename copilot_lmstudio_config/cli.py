# /// script
# requires-python = ">=3.13"
# dependencies = [
#     "requests",
#     "click",
#     "json5",
# ]
# ///
"""
LM Studio to GitHub Copilot Custom Models Generator

This script automatically discovers LLM models from your LM Studio instance
and generates the configuration needed for GitHub Copilot's custom OpenAI models feature.

Features:
- Auto-discovery of all available LLM models
- Proper capability detection (tool calling, context length)
- Direct VS Code settings.json update support
- Cross-platform compatibility (macOS, Windows, Linux)
- JSONC format support (handles comments and trailing commas)

Usage:
    # Install dependencies and run with uv (recommended)
    uv run generate-custom-oai-models.py --help
    
    # Or install manually and run with python
    pip install requests click json5
    python generate-custom-oai-models.py --help

gist: https://github.com/your-username/lm-studio-copilot-config
Author: Alessandro Bologna
License: MIT
"""
import requests
import json
import json5
import click
import shutil
import platform
import difflib
import sys
import os
from collections import OrderedDict
from pathlib import Path
from urllib.parse import urlparse

def get_vscode_settings_path(editor_type):
    """Get the settings.json path for VS Code or VS Code Insiders based on OS."""
    system = platform.system()
    home = Path.home()

    if editor_type == "code":
        if system == "Darwin":  # macOS
            return home / "Library/Application Support/Code/User/settings.json"
        elif system == "Windows":
            appdata = Path(os.environ.get("APPDATA", home / "AppData/Roaming"))
            return appdata / "Code/User/settings.json"
        else:  # Linux and others
            return home / ".config/Code/User/settings.json"
    elif editor_type == "code-insiders":
        if system == "Darwin":  # macOS
            return home / "Library/Application Support/Code - Insiders/User/settings.json"
        elif system == "Windows":
            appdata = Path(os.environ.get("APPDATA", home / "AppData/Roaming"))
            return appdata / "Code - Insiders/User/settings.json"
        else:  # Linux and others
            return home / ".config/Code - Insiders/User/settings.json"
    else:
        raise ValueError(f"Unknown editor type: {editor_type}")

def detect_indentation(content):
    """Detect the indentation style from existing content."""
    for line in content.splitlines():
        if line and (line[0] == ' ' or line[0] == '\t'):
            # Found an indented line, extract the leading whitespace
            indent = ''
            for char in line:
                if char in (' ', '\t'):
                    indent += char
                else:
                    break
            if indent:
                return len(indent)
    # Default to 2 spaces if we can't detect
    return 2

def show_diff_and_confirm(old_content, new_content, file_path):
    """Show diff between old and new content and ask for confirmation.

    Returns: 'unchanged', 'apply', or 'cancel'.
    """
    old_lines = old_content.splitlines(keepends=True)
    new_lines = new_content.splitlines(keepends=True)

    # Use ndiff but only keep changed lines (+/-), mirroring the Rust behavior.
    diff = list(difflib.ndiff(old_lines, new_lines))
    changes = [line for line in diff if line and line[0] in ('+', '-')]

    if not changes:
        print("No changes detected.")
        return 'unchanged'

    print(f"\nDiff preview for: {file_path}\n")
    for line in changes:
        if line[0] == '+':
            # Green for additions
            print(f"\033[32m{line}\033[0m", end='')
        elif line[0] == '-':
            # Red for deletions
            print(f"\033[31m{line}\033[0m", end='')
    print()

    # Ask for confirmation
    response = input("\nApply these changes? [y/N]: ").strip().lower()
    if response in ['y', 'yes']:
        return 'apply'
    return 'cancel'

def fetch_models(api_base):
    resp = requests.get(api_base)
    resp.raise_for_status()
    return resp.json()["data"]

def generate_copilot_config(api_base, openai_url):
    models = fetch_models(api_base)
    config = {}
    for model in models:
        # Only include LLM models, skip embeddings and other types
        if model.get("type") not in ["llm", "vlm"]:
            continue
            
        model_id = model["id"]
        capabilities = model.get("capabilities", [])
        max_context = model.get("max_context_length", 8192)
        
        # Insert fields; we'll normalize key order below
        config[model_id] = {
            "name": model_id,
            "url": openai_url,
            "toolCalling": "tool_use" in capabilities,
            "vision": "vision" in capabilities,  # Check for vision capability
            "thinking": True,  # Default to True, can be customized per model
            "maxInputTokens": max_context,
            "maxOutputTokens": max_context,
            "requiresAPIKey": False,
        }

    # Sort models by id for stable ordering (to match Rust BTreeMap),
    # and sort fields alphabetically within each model (to match serde_json Map)
    ordered: dict[str, dict] = {}
    for model_id in sorted(config.keys()):
        v = config[model_id]
        ordered[model_id] = OrderedDict(sorted(v.items(), key=lambda item: item[0]))

    return ordered

def update_settings_file(settings_path, config):
    """Update the settings.json file with the new model configuration."""
    settings_file = Path(settings_path).expanduser()

    # Read existing content
    old_content = ""
    if settings_file.exists():
        try:
            # Use json5 to parse JSONC files (handles comments and trailing commas)
            with open(settings_file, 'r', encoding='utf-8') as f:
                old_content = f.read()

            settings = json5.loads(old_content)

        except Exception as e:
            # If parsing fails, create a minimal settings structure
            print(f"⚠️  Could not parse existing settings ({e}), creating new structure...")
            settings = {}
    else:
        settings = {}

    # Detect original indentation
    indent = detect_indentation(old_content) if old_content else 2

    # Update the customOAIModels section
    settings["github.copilot.chat.customOAIModels"] = config

    # Generate new content with original indentation
    new_content = json.dumps(settings, indent=indent)

    # Show diff and ask for confirmation
    decision = show_diff_and_confirm(old_content, new_content, str(settings_file))
    if decision == 'unchanged':
        # Nothing to do, leave file and backups untouched.
        return
    if decision == 'cancel':
        print("❌ Operation cancelled by user")
        sys.exit(0)

    # Create dated backup before modifying, e.g. settings.250924-0.backup.json
    if settings_file.exists():
        from datetime import datetime

        date_tag = datetime.now().strftime("%y%m%d")
        stem = settings_file.stem or "settings"

        index = 0
        while True:
            backup_name = f"{stem}.{date_tag}-{index}.backup.json"
            backup_path = settings_file.with_name(backup_name)
            if not backup_path.exists():
                break
            index += 1

        shutil.copy2(settings_file, backup_path)
        print(f"📋 Created backup at {backup_path}")

    # Write back to file (as regular JSON with proper formatting)
    with open(settings_file, 'w', encoding='utf-8') as f:
        f.write(new_content)

    print(f"✅ Updated {settings_file} with {len(config)} models")


@click.command(context_settings={"help_option_names": ["-h", "--help"]})
@click.option(
    '--base-url',
    metavar='BASE_URL',
    default='http://localhost:3000/v1',
    show_default=True,
    help='Base URL to write in VS Code config (where Copilot will connect)',
)
@click.option(
    '--lmstudio-url',
    metavar='LMSTUDIO_URL',
    default=None,
    help='LM Studio URL to fetch models from (defaults to base-url with port 1234)',
)
@click.option(
    '--settings',
    metavar='SETTINGS',
    type=click.Choice(['code', 'code-insiders'], case_sensitive=False),
    help='Auto-detect VS Code settings path (code or code-insiders)',
)
@click.option(
    '--settings-path',
    metavar='SETTINGS_PATH',
    type=click.Path(),
    help='Path to VS Code settings.json file (prints to stdout if not provided)',
)
def main(base_url, lmstudio_url, settings, settings_path):
    """
    Generate GitHub Copilot custom OpenAI models configuration from LM Studio + proxy.
    
    This script automatically discovers all LLM models available in your LM Studio instance
    and generates the proper configuration for GitHub Copilot's custom OpenAI models feature.
    It reads model capabilities (tool calling, context length) directly from the API.
    
    \b
    EXAMPLES:
    
    # Generate config and print to stdout (copy/paste into VS Code settings)
    uv run generate-custom-oai-models.py
    
    # Use custom proxy + LM Studio URLs
    uv run generate-custom-oai-models.py \\
        --base-url http://studio.local:3000/v1 \\
        --lmstudio-url http://studio.local:1234
    
    # Update VS Code settings file directly (macOS)
    uv run generate-custom-oai-models.py --settings-path "~/Library/Application Support/Code/User/settings.json"
    
    # Update VS Code Insiders settings (macOS)  
    uv run generate-custom-oai-models.py --settings-path "~/Library/Application Support/Code - Insiders/User/settings.json"
    
    # Windows VS Code settings
    uv run generate-custom-oai-models.py --settings-path "%APPDATA%/Code/User/settings.json"
    
    # Linux VS Code settings
    uv run generate-custom-oai-models.py --settings-path "~/.config/Code/User/settings.json"
    
    \b
    SETUP:
    1. Start LM Studio with your desired models loaded
    2. Run this script to generate or update your configuration
    3. Restart VS Code to pick up the new models
    4. Access your local models via GitHub Copilot chat model selector
    
    The script automatically detects tool calling capabilities, context lengths, and filters
    out non-LLM models (like embeddings). All models are configured with thinking=true
    and vision=false by default (adjust manually if needed).
    """
    
    # Validate options
    if settings and settings_path:
        click.echo("Error: Cannot use both --settings and --settings-path", err=True)
        sys.exit(1)

    # Determine the settings path
    final_settings_path = None
    if settings:
        try:
            final_settings_path = str(get_vscode_settings_path(settings))
            print(f"Using settings file: {final_settings_path}")
        except ValueError as e:
            click.echo(f"Error: {e}", err=True)
            sys.exit(1)
    elif settings_path:
        final_settings_path = settings_path

    # Construct API URLs
    if lmstudio_url:
        lmstudio_base = lmstudio_url.rstrip('/')
    else:
        # Derive LM Studio URL from base-url by swapping the port to 1234
        base = base_url.rstrip('/')
        if base.endswith('/v1'):
            base = base[:-3].rstrip('/')

        parsed = urlparse(base)
        if parsed.scheme and parsed.hostname:
            lmstudio_base = f"{parsed.scheme}://{parsed.hostname}:1234"
        else:
            lmstudio_base = "http://localhost:1234"

    api_base = f"{lmstudio_base}/api/v0/models"
    openai_url = base_url

    try:
        config = generate_copilot_config(api_base, openai_url)

        if final_settings_path:
            # Update the settings file directly
            update_settings_file(final_settings_path, config)
        else:
            # Print the configuration to stdout
            output = {"github.copilot.chat.customOAIModels": config}
            print(json.dumps(output, indent=2))
            
    except requests.exceptions.RequestException as e:
        target = lmstudio_url or lmstudio_base
        click.echo(f"❌ Error connecting to LM Studio API at {target}: {e}", err=True)
        exit(1)
    except Exception as e:
        click.echo(f"❌ Error: {e}", err=True)
        exit(1)


if __name__ == "__main__":
    main()
