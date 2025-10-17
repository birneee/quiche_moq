{
  description = "Turbo Quiche";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        buildRustPackage =
          name: path:
          let
            manifest = (pkgs.lib.importTOML (./. + "/${path}/Cargo.toml")).package;
          in
          pkgs.rustPlatform.buildRustPackage {
            pname = manifest.name;
            version = manifest.version;
            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes = {
                "octets-0.3.3" = "sha256-9S32I2k1XIt30QVN3CGvCu+OQCpegDNmX6gDePT2L6o=";
                "quiche-0.24.6" = "sha256-9S32I2k1XIt30QVN3CGvCu+OQCpegDNmX6gDePT2L6o=";
                "quiche_endpoint-0.1.0" = "sha256-AUR7Gg/uBifh6/zc3Q4FGXh2506laXRO1q9cqIfARes=";
                "quiche_mio_runner-0.1.0" = "sha256-uvM5+zY+g+vcNHPBsrqevQCRCc6oAH6+7Fbm7k6Aif0=";
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
        packages.moq-utils = buildRustPackage "moq-utils" "moq_utils";
        packages.time-client-example = buildRustPackage "time-client" "quiche_moq/examples/time-client";
        packages.time-server-example = buildRustPackage "time-server" "quiche_moq/examples/time-server";
        packages.video-client-example = buildRustPackage "video-client" "quiche_moq/examples/video-client";
        packages.video-server-example = buildRustPackage "video-server" "quiche_moq/examples/video-server";
        packages.default = self.packages.${system}.moq-utils;
        devShells.default =
          let
            rust-toolchain =
              with pkgs;
              pkgs.symlinkJoin {
                name = "rust-toolchain";
                paths = [
                  rustc
                  cargo
                  rustPlatform.rustcSrc
                ];
              };
          in
          pkgs.mkShell {
            buildInputs = with pkgs; [
              clippy
              rustfmt
              rust-analyzer
              rust-toolchain
            ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
      }
    );
}
