{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay.url = "github:oxalica/rust-overlay";
    # for experiments/
    # with its own nixpkgs: its jail needs a newer util-linux (unshare --forward-signals) than ours
    nixnet.url = "github:birneee/nixnet";
    test-certs.url = "github:birneee/test-certs";
    test-certs.inputs.nixpkgs.follows = "nixpkgs";
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
          inputs',
          system,
          ...
        }:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          toolchainWith =
            targets:
            pkgs.rust-bin.stable."1.92.0".default.override {
              extensions = [
                "rust-src"
                "clippy"
                "rustfmt"
                "rust-analyzer"
              ];
              inherit targets;
            };
          rustToolchain = toolchainWith [ "wasm32-unknown-unknown" ];
          # also builds for a bare-metal target, where only core and alloc exist
          devToolchain = toolchainWith [
            "wasm32-unknown-unknown"
            "thumbv7em-none-eabihf"
          ];
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "quiche_endpoint-0.1.0" = "sha256-jENeNMvcmxRV7gJpKOSC8X4pOmJdL9x6eWU8Xrw5cdE=";
              "quiche_mio_runner-0.1.0" = "sha256-Wi1/6o1j5Iutl2ukykkVNGWOTfqSRlLkqqWOwsEaM5c=";
              "datagram-socket-0.8.0" = "sha256-URJo1AFpuCJNtSKcvo9RIQMyl6XuM6iiFqQq8/otIWo=";
            };
          };
          # Git-tracked files only (drops target/, .direnv/, drafts/, qlog/*.sqlog
          # etc. via .gitignore), minus tracked dirs/files that aren't part of the
          # Cargo build - keeps the src (and its hash) from churning on unrelated
          # changes, e.g. editing interop/ no longer invalidates every crate build.
          cargoSrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.difference (pkgs.lib.fileset.gitTracked ./.) (
              pkgs.lib.fileset.unions [
                ./interop
                ./.github
                ./README.md
                ./logo.svg
                ./flake.nix
                ./flake.lock
                ./experiments
              ]
            );
          };
          # Without this, panic!/#[track_caller] location strings embed
          # the real /nix/store path of both our own (vendored) source
          # and the rustc/rust-std sysroot, and Nix's reference scanner
          # then adds the *entire* ~2GB rust toolchain closure as a
          # runtime dependency of the built binary (verified: moq-cli's
          # closure was 2GB, 99.6% of it rustc/cargo/rust-std/etc. -
          # the actual binary is 8MB). Remapping both to fake,
          # non-store paths stops the scanner from seeing them.
          remapPaths = "--remap-path-prefix=/build=/src --remap-path-prefix=${rustToolchain}=/rust-toolchain";
          buildMember =
            name: path:
            let
              manifest = (pkgs.lib.importTOML (./. + "/${path}/Cargo.toml")).package;
            in
            rustPlatform.buildRustPackage {
              pname = manifest.name;
              version = manifest.version;
              inherit cargoLock;
              src = cargoSrc;
              buildAndTestSubdir = [ path ];
              nativeBuildInputs = with pkgs; [
                clang
                git
                cmake
              ];
              env = {
                LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
                RUSTFLAGS = remapPaths;
              };
            };
          # the version of the wasm-bindgen crate in Cargo.lock, which the CLI must match
          wasmBindgenCli = pkgs.buildWasmBindgenCli rec {
            src = pkgs.fetchCrate {
              pname = "wasm-bindgen-cli";
              version = "0.2.114";
              hash = "sha256-xrCym+rFY6EUQFWyWl6OPA+LtftpUAE5pIaElAIVqW0=";
            };
            cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
              inherit src;
              inherit (src) pname version;
              hash = "sha256-Z8+dUXPQq7S+Q7DWNr2Y9d8GMuEdSnq00quUR0wDNPM=";
            };
          };
          # moq_web/dist as moq_web/make-dist builds it, without wasm-opt
          webPlayer = rustPlatform.buildRustPackage {
            pname = "moq_web";
            version = (pkgs.lib.importTOML ./moq_web/Cargo.toml).package.version;
            inherit cargoLock;
            src = cargoSrc;
            nativeBuildInputs = [ wasmBindgenCli ];
            # RUSTFLAGS replaces the rustflags of .cargo/config.toml
            env.RUSTFLAGS = "--cfg=web_sys_unstable_apis ${remapPaths}";
            buildPhase = ''
              runHook preBuild
              cargo build --release --offline --target wasm32-unknown-unknown -p moq_web
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              wasm-bindgen --target web --out-dir $out target/wasm32-unknown-unknown/release/moq_web.wasm
              cp moq_web/index.html $out/
              cp ${statsJs} $out/stats.min.js
              runHook postInstall
            '';
            doCheck = false;
          };
          # stats.js r17 for the web player's stats panels, as moq_web/make-dist fetches it
          statsJs = pkgs.fetchurl {
            url = "https://raw.githubusercontent.com/mrdoob/stats.js/5f0f917354ea2184456e5e361da5df6166306fbf/build/stats.min.js";
            sha256 = "8a2c2b2d6e1250186bcd6adf5b65593fff66a301d7f8ab3510a39116fe041dd1";
          };
          interopImages = import ./interop/interop.nix {
            inherit pkgs;
            moq-cli = self'.packages.moq-cli;
          };
          # the interop matrix of every implementation, one derivation per pair
          interopSandbox = import ./interop/sandbox {
            inherit pkgs;
            ourImage = self'.packages.interop-image;
          };
        in
        {
          packages.moq-cli = buildMember "moq-cli" "moq_cli";
          packages.time-client-example = buildMember "time-client" "quiche_moq/examples/time-client";
          packages.time-server-example = buildMember "time-server" "quiche_moq/examples/time-server";
          packages.video-client-example = buildMember "video-client" "quiche_moq/examples/video-client";
          packages.video-server-example = buildMember "video-server" "quiche_moq/examples/video-server";
          packages.web-player = webPlayer;
          packages.default = self'.packages.moq-cli;
          packages.interop-image = interopImages.image;
          packages.interop-matrix = interopSandbox.matrix;
          # e.g. nix build .#interop.pairs.moq-rs-draft-18--birneee-moq
          legacyPackages.interop = interopSandbox;
          # e.g. nix run .#experiments.web
          legacyPackages.experiments = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux (import ./experiments {
            inherit pkgs;
            nixnet = inputs'.nixnet.legacyPackages;
            certs = inputs'.test-certs.packages.default;
            moq-cli = self'.packages.moq-cli;
            webPlayer = self'.packages.web-player;
          });
          devShells.default = pkgs.mkShell {
            inputsFrom = [
              self'.packages.default
            ];
            packages = [
              devToolchain
              pkgs.ffmpeg
              pkgs.wasm-pack
            ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            shellHook = ''
              # Symlink for IDEs
              ln -sfn ${devToolchain} $PWD/.rust-toolchain
            '';
          };
        };
    };
}
