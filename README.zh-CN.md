# cadmus

[English](./README.md)（如有出入以英文版为准）

TODO: 一句话说明这个项目做什么（同步更新 crates/cadmus/Cargo.toml 的 description）。

## 安装

首次发版后可使用 GitHub Release 的安装脚本：

    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/duskgrow/cadmus/releases/latest/download/cadmus-installer.sh | sh

或从 crates.io 安装：

    cargo install cadmus

## 使用

见 `cadmus --help`。

## 开发

    direnv allow   # 或 nix develop（进入 devShell 会自动安装 git 钩子）
    just ci        # 全量质量门（本地绿 ≡ CI 绿）

详见 [CONTRIBUTING.zh-CN.md](./CONTRIBUTING.zh-CN.md)。

## License

MIT OR Apache-2.0，见 [LICENSE-MIT](./LICENSE-MIT) 与 [LICENSE-APACHE](./LICENSE-APACHE)。
