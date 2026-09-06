# InTools

InTools is a desktop tool host program based on a plugin mechanism. It aggregates the capabilities of various tools (plugins) through a unified protocol layer, providing a concise user interface and permission control.

## Core Features

- **Plugin Architecture**: Any executable program or script (e.g., Python, Node.js, binary) can be connected as a plugin.
- **Unified Communication Protocol**: Inter-process communication based on JSON-RPC 2.0 ensures interactions between plugins and the host are clear and predictable.
- **Permission Model**: Fine-grained permission control across 19 categories. Plugins must declare required permissions, and users can choose to grant or deny them.
- **Global Shortcuts**: Supports configuring global hotkeys for plugins, which can be triggered even without window focus.
- **AI Orchestration Integration**: Built-in AI orchestration plugin that can call LLM APIs to combine multiple tools.
- **Plugin Import**: Import third-party plugins from zip packages directly into InTools.
- **MCP Gateway**: When enabled, provides an HTTP endpoint for other MCP clients to interact with InTools plugins.
- **Clipboard History**: Background clipboard monitoring, press `Ctrl+Shift+V` to view history and quick-paste.
- **Declarative UI**: Plugins declare UI via manifest, host renders uniformly with grouped forms and structured result display.
- **Cross-platform Support**: Built on Tauri 2.11.5, supporting Windows, macOS, and Linux.

## Directory Structure

```
in-tools/
├── src-tauri/              # Tauri Host (Rust)
│   ├── src/
│   │   ├── commands.rs     # Frontend interaction commands
│   │   ├── config/         # Config management
│   │   ├── gateway.rs      # MCP Gateway
│   │   ├── hotkey.rs       # Global shortcuts
│   │   ├── mcp/           # MCP protocol implementation
│   │   ├── permission/    # Permission management
│   │   ├── protocol/      # Plugin communication protocol
│   │   ├── registry/      # Plugin registry
│   │   ├── runtime/       # Runtime management
│   │   ├── shortcut/      # Shortcut parsing
│   │   └── ui.rs          # UI related
│   └── tests/             # Integration tests
├── plugins/                # Sample plugins (Python)
│   ├── ai-orchestrator/   # AI orchestration plugin
│   ├── clipboard-tool/    # Clipboard access plugin
│   ├── color-picker/      # Color picker plugin
│   ├── file-search/       # File search tool
│   ├── hello-plugin/      # Beginner sample plugin
│   ├── ocr-tool/          # OCR recognition plugin
│   ├── screenshot-plugin/ # Screenshot tool
│   └── window-info/       # Window information plugin
├── src/                   # Frontend resources
│   ├── index.html         # Main interface
│   ├── main.js           # Frontend logic
│   ├── style.css         # Styles
│   └── overlay.*         # Screenshot overlay
├── docs/                  # Documentation
│   ├── plugin-development.md  # Plugin development guide
│   └── superpowers/specs/     # Design documents
```

## Quick Start

### Installation

Download the installer for your platform from [Gitee releases](https://gitee.com/buxiaju/in-tools/releases), or compile from source:

```bash
cd src-tauri
cargo tauri build
```

### Usage

1. Start InTools; the taskbar icon displays the application status.
2. Switch function views via the left navigation.
3. Plugin View: Manage the enable/disable status of plugins.
4. AI Chat: Interact with AI, which can automatically call plugin tools.
5. Permissions View: Manage granted permissions.
6. Plugin Dev: View development documentation.
7. Settings View: Configure log levels, AI APIs, close behavior, MCP gateway toggle, etc.

## Plugin Development

### Plugin Structure

Each plugin is a directory containing a `manifest.toml` description file:

```
my-plugin/
├── manifest.toml        # Plugin metadata and tool declarations
├── main.py             # Entry script (or other executable)
└── README.md           # Usage instructions (optional)
```

### manifest.toml Example

```toml
id = "my-plugin"
name = "My Plugin"
version = "1.0.0"

[command]
type = "python"
args = ["main.py"]

[[tools]]
name = "hello"
description = "Say hello"
input_schema = { type = "object", properties = { name = { type = "string" } } }

[permissions]
declare = ["file.read"]

[shortcut]
key = "Ctrl+Shift+H"
description = "Quick greeting"
```

### Protocol Communication

Plugins communicate with the host via JSON-RPC 2.0 over standard input/output (stdin/stdout):

```python
import sys
import json

def main():
    # 1. Active handshake
    print(json.dumps({"jsonrpc": "2.0", "method": "hello", "params": {"protocol_version": "1.0"}}))
    sys.stdout.flush()
    
    # 2. Main loop: read requests and respond
    for line in sys.stdin:
        request = json.loads(line)
        # Handle request["method"], return result
```

For detailed protocol specifications, please refer to the [Plugin Development Manual](./docs/plugin-development.md).

### Existing Plugin References

- **hello-plugin**: Minimal plugin template, demonstrating basic structure.
- **file-search**: File search tool, demonstrating parameter handling and path security.
- **screenshot-plugin**: Screenshot capture, demonstrating system-level API calls.
- **ai-orchestrator**: AI orchestration, demonstrating multi-tool combination and LLM calls.
- **window-info**: Window information retrieval.
- **ocr-tool**: OCR recognition for extracting text from images.
- **clipboard-tool**: Clipboard read/write access.
- **color-picker**: Screen color picking.

## Configuration

### Main Config

`~/.intools/host-config.json`:

```json
{
    "plugins_dir": null,
    "log_level": "info",
    "close_behavior": "minimize",
    "mcp_enabled": false,
    "mcp_token": ""
}
```

### Shortcut Config

`~/.intools/shortcuts.json`:

```json
{
    "hello-plugin": {
        "key": "Ctrl+Shift+H",
        "enabled": true
    }
}
```

## Permission Model

Plugins must declare required permissions in `manifest.toml`. The host supports 19 permission categories:

| Category | Actions | Description |
|---|---|---|
| file | read / write | File system access |
| network | http / websocket / dns / socket / read / write / send / receive | Network access |
| process | spawn | Spawn child processes |
| shell | exec | Execute shell commands |
| screen | capture / record | Screen capture and recording |
| input | control | Simulate keyboard and mouse input |
| clipboard | read / write | Read/write clipboard |
| audio | capture / record / send / receive | Microphone and speakers |
| system | manage | Shutdown / restart / logout / lock screen |
| window | manage / modify | Manipulate other windows |
| app | spawn | Launch desktop applications |
| registry | read / write / modify | Windows registry access |
| credential | read / write | Credential store access |
| crypto | read / write | Key / certificate access |
| notification | send | Send system notifications |
| hardware | read / write / control | Camera / Bluetooth / serial port etc. |
| persistence | install / uninstall / modify | Install / uninstall / auto-start |
| schedule | manage | Scheduled tasks / cron |
| environment | read / write | Process environment variables |

When using a plugin requiring high-risk permissions for the first time, users will receive a prompt and can choose:
- **Allow**: Valid for this invocation only.
- **Always Allow**: Writes persistent authorization.
- **Deny**: This invocation is intercepted.

## FAQ

**Q: Plugin fails to start?**
A: Check `stderr` logs (view logs in settings), confirm the Python environment is correct, and dependencies are installed.

**Q: Shortcuts not working?**
A: Shortcuts are registered only after the plugin is enabled; check if another program is occupying the key combination.

**Q: How to debug plugins?**
A: Use `print()` in the plugin's `main.py` to output logs to `stderr`, or manually simulate protocol interaction.

## Build & Development

### Environment Dependencies

- Rust 1.88+
- Python 3.8+ (for developing Python plugins)
- Node.js (optional, for frontend resources)

### Local Development

```bash
# Clone repository
git clone https://gitee.com/buxiaju/in-tools.git
cd in-tools

# Start development server
cd src-tauri
cargo tauri dev
```

### Run Tests

```bash
cd src-tauri
cargo test --lib
```

## Protocol & Compatibility

Current Protocol Version: **1.0**

If major versions are incompatible, plugins cannot be loaded. Minor version differences are backward compatible.

## License

This project uses the [MIT License](./LICENSE) open-source agreement.

## Contribution

Issues and Pull Requests are welcome. When developing plugins, please read the [Plugin Development Manual](./docs/plugin-development.md) first to understand the detailed specifications.
