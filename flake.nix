{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
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
      eachSystem = nixpkgs.lib.genAttrs supportedSystems;

      rustTargetFor =
        pkgs:
        if pkgs.stdenv.hostPlatform.isWindows then
          "x86_64-pc-windows-gnu"
        else
          {
            x86_64-linux = "x86_64-unknown-linux-musl";
            aarch64-linux = "aarch64-unknown-linux-musl";
          }
          .${pkgs.stdenv.hostPlatform.system} or null;

      toolchainFor =
        rustTarget: pkgs:
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
          mkPackage =
            { crossSystem ? null }:
            let
              pkgs = import nixpkgs (
                {
                  localSystem = system;
                  inherit overlays;
                }
                // nixpkgs.lib.optionalAttrs (crossSystem != null) {
                  inherit crossSystem;
                }
              );
              rustTarget = rustTargetFor pkgs;
              craneLib = (crane.mkLib pkgs).overrideToolchain (toolchainFor rustTarget);
              src = pkgs.lib.cleanSourceWith {
                src = ./.;
                filter =
                  path: _:
                  let
                    relative = pkgs.lib.removePrefix "${toString ./.}/" (toString path);
                  in
                  relative == "Cargo.toml"
                  || relative == "Cargo.lock"
                  || relative == "build.rs"
                  || relative == "proto"
                  || pkgs.lib.hasPrefix "proto/" relative
                  || relative == "src"
                  || pkgs.lib.hasPrefix "src/" relative;
              };
              craneArgs = {
                inherit src;
                cargoTomlContents = builtins.readFile ./Cargo.toml;
                cargoLockContents = builtins.readFile ./Cargo.lock;
                strictDeps = true;
                SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
              }
              // pkgs.lib.optionalAttrs (rustTarget != null) {
                CARGO_BUILD_TARGET = rustTarget;
              }
              // pkgs.lib.optionalAttrs (
                pkgs.stdenv.hostPlatform.isLinux || pkgs.stdenv.hostPlatform.isWindows
              ) {
                CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
              }
              // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isWindows {
                doCheck = false;
              }
              // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
                RUSTFLAGS = "-C link-arg=-Wl,-dead_strip_dylibs";
              };
              cargoArtifacts = craneLib.buildDepsOnly craneArgs;
            in
            craneLib.buildPackage (
              craneArgs
              // {
                inherit cargoArtifacts;
                meta.mainProgram = if pkgs.stdenv.hostPlatform.isWindows then "brainpod.exe" else "brainpod";
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
        in
        {
          default = mkPackage {
            crossSystem =
              {
                x86_64-linux.config = "x86_64-unknown-linux-musl";
                aarch64-linux.config = "aarch64-unknown-linux-musl";
              }
              .${system} or null;
          };
        }
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
          windows = mkPackage {
            crossSystem = {
              config = "x86_64-w64-mingw32";
              libc = "msvcrt";
            };
          };
        }
      );

      devShells = eachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system overlays; };
          rustTarget = rustTargetFor pkgs;
          craneLib = (crane.mkLib pkgs).overrideToolchain (toolchainFor rustTarget);
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
