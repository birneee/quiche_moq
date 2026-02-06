{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };
  outputs =
    inputs@{
      nixpkgs,
      flake-parts,
      rust-overlay,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      perSystem =
        {
          self',
          system,
          ...
        }:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.stable."1.92.0".default.override {
            extensions = [
              "rust-src"
              "clippy"
              "rustfmt"
              "rust-analyzer"
            ];
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          buildMember =
            name: path:
            let
              manifest = (pkgs.lib.importTOML (./. + "/${path}/Cargo.toml")).package;
            in
            rustPlatform.buildRustPackage {
              pname = manifest.name;
              version = manifest.version;
              cargoLock = {
                lockFile = ./Cargo.lock;
                outputHashes = {
                  "octets-0.3.4" = "sha256-s8arMNx469KgQT4llFOxXW/OVTeqPH/235yml9skuLY=";
                  "quiche_endpoint-0.1.0" = "sha256-j+/0UKKS2uQ4guGx3JFqy0vxgISLY5fGtlF8w7Xj5/I=";
                  "quiche_mio_runner-0.1.0" = "sha256-a3wIvAmonFX8l35uu6p8veCxz5wzh0WbU6zKt7PntCU=";
                };
              };
              src = pkgs.lib.cleanSource ./.;
              buildAndTestSubdir = [ path ];
              nativeBuildInputs = with pkgs; [
                clang
                git
                cmake
              ];
              env = {
                LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              };
            };
        in
        {
          packages.moq-utils = buildMember "moq-utils" "moq_utils";
          packages.time-client-example = buildMember "time-client" "quiche_moq/examples/time-client";
          packages.time-server-example = buildMember "time-server" "quiche_moq/examples/time-server";
          packages.video-client-example = buildMember "video-client" "quiche_moq/examples/video-client";
          packages.video-server-example = buildMember "video-server" "quiche_moq/examples/video-server";
          packages.default = self'.packages.moq-utils;
          devShells.default = pkgs.mkShell {
            inputsFrom = [ self'.packages.default ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            shellHook = ''
              # Symlink for IDEs
              ln -sfn ${rustToolchain} $PWD/.rust-toolchain
            '';
          };
        };
    };
}
