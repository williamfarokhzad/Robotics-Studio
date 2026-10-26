# Building and running

```sh
cargo run --release -p solidcraft -- --sample          # desktop app with a sample part
cargo run --release -p solidcraft-cli -- commands      # the command registry
cargo run --release -p solidcraft-cli -- run examples/bracket.json --out bracket.step
cargo run --release -p solidcraft-cli -- snapshot examples/bracket.json --out bracket.png
cargo run --release -p solidcraft-cli -- snapshot part.solidcraft --out preview.png --tool Pin --section z=12
cargo run --release -p solidcraft-cli -- render examples/render-studio.json --out product.png
cargo run --release -p solidcraft -- part.step          # open a STEP file as a new design
cargo run --release -p solidcraft-cli -- eval part.step # measure an imported STEP file
cargo xtask ci                                          # fmt, clippy, tests, icons, assets, layers, wasm
```

UI artwork changes follow the [icon specification](../assets/icons/ui/STYLE_GUIDE.md).
The [included review tool](../assets/icons/ui/README.md) discovers new icons and keeps
unaccepted artwork Open; run `cargo xtask icons` to check the collection.

On Windows, install Rust 1.95+ with the MSVC toolchain and Visual Studio C++ Build Tools
(including a Windows SDK), then build both executables:

```powershell
cargo build --release --locked -p solidcraft -p solidcraft-cli
.\target\release\solidcraft.exe --control 7878
```

In a second terminal, start the MCP bridge:

```powershell
.\target\release\solidcraft-cli.exe mcp --connect 127.0.0.1:7878
```

The Windows portable ZIP includes both executables; extract the whole ZIP before running.
A standalone desktop EXE does not include the CLI. From a source checkout,
`run-robotics-studio.bat --control 7878` finds an existing desktop executable or builds
both binaries using native Cargo. Extra arguments are forwarded to the app.

Cross-compiling Windows binaries from Linux uses
`cargo xwin build --release --target x86_64-pc-windows-msvc -p solidcraft -p solidcraft-cli`.
The MCP bridge uses stdio; it is not an HTTP endpoint. A stdio MCP client or compatible
tunnel must launch `solidcraft-cli.exe` with arguments `mcp --connect 127.0.0.1:7878`
and own its stdin/stdout. It cannot attach to an unrelated CLI process already running
in another PowerShell window. Keep the desktop app running; let the configured client
start the CLI. The command above is also useful for manual protocol diagnostics.
Connecting ChatGPT requires the additional compatible tunnel or HTTP transport adapter;
this repository does not currently ship that adapter. See [MCP](mcp.md).


In the browser: `cd apps/solidcraft-web && trunk build --release --public-url ./` (WebGPU, or
WebGL2 with `?webgl`; `?sample` opens the sample part).

Agents can control the document open in a browser tab through `window.solidcraft.execute(...)`
and `window.solidcraft.request(...)`; see the [browser command API](browser-api.md).

See [CONTRIBUTING](../CONTRIBUTING.md) for the test and review workflow.
