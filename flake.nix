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
            echo "devShell ready. Entry point: just --list (git hooks arm themselves on shell entry)"
          '';
        };

        # >>> template-only: one-command bootstrap — `nix run <flake> -- <name> <owner>`
        # copies the template into an empty cwd and runs the same xtask init
        # subcommand (runtimeInputs carry git/cargo/prek, no `nix develop` needed);
        # removed by `just init`
        apps.default = flake-utils.lib.mkApp {
          drv = pkgs.writeShellApplication {
            name = "rust-template-init";
            runtimeInputs = with pkgs; [ bash git coreutils prek rustToolchain ];
            text = ''
              if [ "$#" -ne 2 ]; then
                  echo "usage: nix run <template-flake> -- <project-name> <github-owner>" >&2
                  exit 2
              fi
              if [ -n "$(ls -A . 2>/dev/null)" ]; then
                  echo "error: run this in an empty directory — it becomes your project root" >&2
                  exit 1
              fi
              cp -r ${self}/. .
              chmod -R u+w .  # store copies are read-only; must precede any rm
              # A `path:` flake on a commit-less tree copies the raw directory —
              # including any local .git (init would then skip `git init` and build
              # on the template's own repo state) and build artifacts. Pristine required.
              rm -rf .git target .direnv result
              exec cargo run -q -p xtask -- init "$1" "$2"
            '';
          };
        };
        # <<< template-only
      })
    # >>> template-only: distribution entry for `nix flake init`; removed by `just init`
    // {
      templates.default = {
        path = ./.;
        description = "Rust project template: workspace + just + nextest + insta + release-plz + cargo-dist + OIDC Trusted Publishing";
        welcomeText = ''

          rust-template has been copied to the current directory (no git history).
          Next step:
            nix develop -c just init <project-name> <github-owner>
        '';
      };
    }
    # <<< template-only
    ;
}
