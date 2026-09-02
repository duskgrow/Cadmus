# cadmus

[English](./README.md)（如有出入以英文版为准）

用 Rust 编写的自进化 coding agent：自研 agent 循环、OpenAI 兼容的 provider 方言层、只读编码工具集——当前处于[路线图](docs/roadmap.md)的阶段 0。

## 安装

首次发版后可使用 GitHub Release 的安装脚本：

    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/duskgrow/cadmus/releases/latest/download/cadmus-installer.sh | sh

或从 crates.io 安装：

    cargo install cadmus

## 使用

    export MOONSHOT_API_KEY=sk-…   # 或 DEEPSEEK_API_KEY（配合 --provider deepseek）
    cadmus chat "解释一下 crates/cadmus-core/src/agent.rs"

编码工具（`read_file`、`grep`、`list_dir`）只读且限制在当前目录内。完整选项见 `cadmus --help`。

## 开发

    direnv allow   # 或 nix develop（进入 devShell 会自动安装 git 钩子）
    just ci        # 全量质量门（本地绿 ≡ CI 绿）

详见 [CONTRIBUTING.zh-CN.md](./CONTRIBUTING.zh-CN.md)。

## License

MIT OR Apache-2.0，见 [LICENSE-MIT](./LICENSE-MIT) 与 [LICENSE-APACHE](./LICENSE-APACHE)。
