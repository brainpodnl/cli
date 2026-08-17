{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    nix-filter.url = "github:numtide/nix-filter";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    {
      nixpkgs,
      crane,
      rust-overlay,
      nix-filter,
      ...
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      overlays = [ (import rust-overlay) ];
      filter = import nix-filter;
      eachSystem = nixpkgs.lib.genAttrs supportedSystems;

      rustTargetFor =
        system:
        {
          x86_64-linux = "x86_64-unknown-linux-musl";
          aarch64-linux = "aarch64-unknown-linux-musl";
        }
        .${system} or null;

      toolchainFor =
        pkgs:
        let
          rustTarget = rustTargetFor pkgs.stdenv.hostPlatform.system;
        in
        pkgs.rust-bin.nightly.latest.minimal.override (
          {
            extensions = [
              "clippy"
              "rust-src"
            ];
          }
          // pkgs.lib.optionalAttrs (rustTarget != null) {
            targets = [ rustTarget ];
          }
        );
    in
    {
      packages = eachSystem (
        system:
        let
          rustTarget = rustTargetFor system;
          pkgs = import nixpkgs (
            {
              localSystem = system;
              inherit overlays;
            }
            // nixpkgs.lib.optionalAttrs (rustTarget != null) {
              crossSystem.config = rustTarget;
            }
          );
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchainFor;
          src = filter {
            root = ./.;
            include = [
              "src"
              "Cargo.toml"
              "Cargo.lock"
            ];
          };
          craneArgs = {
            inherit src;
            strictDeps = true;
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            CARGO_BUILD_TARGET = rustTarget;
            CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
            RUSTFLAGS = "-C link-arg=-Wl,-dead_strip_dylibs";
          };
          cargoArtifacts = craneLib.buildDepsOnly craneArgs;
        in
        {
          default = craneLib.buildPackage (
            craneArgs
            // {
              inherit cargoArtifacts;
              pname = "brainpod-cli";
              version = "0.1.0";
              meta.mainProgram = "brainpod";
            }
            // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              nativeBuildInputs = [ pkgs.buildPackages.binutils ];
              postFixup = ''
                if readelf -l "$out/bin/brainpod" | grep -F INTERP; then
                  echo "brainpod contains a dynamic ELF interpreter" >&2
                  exit 1
                fi
                if readelf -d "$out/bin/brainpod" | grep -F '(NEEDED)'; then
                  echo "brainpod contains a dynamic library dependency" >&2
                  exit 1
                fi
              '';
            }
            // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
              postFixup = ''
                if otool -L "$out/bin/brainpod" | tail -n +2 | grep -F /nix/store/; then
                  echo "brainpod contains a dynamic Nix store dependency" >&2
                  exit 1
                fi
              '';
            }
          );
        }
      );

      devShells = eachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system overlays; };
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchainFor;
        in
        {
          default = craneLib.devShell {
            packages = with pkgs; [
              rustfmt
              rust-analyzer
            ];
          };
        }
      );
    };
}
