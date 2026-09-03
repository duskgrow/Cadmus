{
  description = "Rust project dev environment (toolchain SSOT lives in rust-toolchain.toml; this file only consumes it)";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs"; # avoid a second nixpkgs copy in flake.lock
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          # Responsibility boundary: flake provides toolchains and ecosystem
          # tools; language dependencies belong to cargo (Cargo.lock).
          # No 2nix tooling (crane/naersk/...) in the dev shell.
          packages = [
            rustToolchain # cargo/rustc/rustfmt/clippy/rust-analyzer — version: rust-toolchain.toml
          ] ++ (with pkgs; [
            just # task-layer entry point
            dprint # markdown formatter (config: dprint.json)
            prek # git hook runner (reads .pre-commit-config.yaml)
            cargo-nextest # test runner (process isolation / sharding)
            cargo-insta # snapshot review
            cargo-deny # dependency policy: advisories/licenses/bans/sources
            cargo-semver-checks # public-API breakage gate (pre-release)
            cargo-dist # cross-platform release; keep in sync with dist-workspace.toml
            actionlint # static analysis for GitHub Actions workflows
            nixd # Nix language server
            nil # Nix language server (alternative — an editor uses one of the two)
          ]);

          shellHook = ''
            # Git hooks cannot travel with a clone (git's security model), so the
            # devShell arms them on entry — idempotent, silent, and skipped in CI
            # and before `just init` creates .git. Non-Nix shells: `just setup`.
            if [ -e .git ] && [ -z "''${CI:-}" ]; then
              prek install --hook-type pre-commit --hook-type commit-msg >/dev/null 2>&1 || true
            fi
            # Banner goes to stderr: stdout of `nix develop -c <cmd>` must stay
            # clean for output-capturing callers (rust-cache's cmd-format parses it).
            echo "devShell ready. Entry point: just --list (git hooks arm themselves on shell entry)" >&2
          '';
        };

      })
    ;
}
