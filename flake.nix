{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    systems.url = "github:nix-systems/default";
    nix-filter.url = "github:numtide/nix-filter";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    systems,
    ...
  }: let
    eachSystem = nixpkgs.lib.genAttrs (import systems);
    overlays = [(import rust-overlay)];

    rustBinFor = pkgs: pkgs.rust-bin.nightly.latest;
  in {
    devShells = eachSystem (system: let
      pkgs = import nixpkgs {inherit system overlays;};
      rust-bin = rustBinFor pkgs;
    in {
      default = pkgs.mkShell {
        nativeBuildInputs = with rust-bin; [
          (minimal.override {
            extensions = ["clippy" "rust-src"];
          })
        ];

        packages = with pkgs; [
          rustfmt
          rust-analyzer
        ];
      };
    });
  };
}
