{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixnet.url = "github:birneee/nixnet";
    test-certs.url = "github:birneee/test-certs";
  };
  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      perSystem =
        { pkgs, inputs', ... }:
        let
          nixnet = inputs'.nixnet.legacyPackages;
          certs = inputs'.test-certs.packages.default;
          fontsConf = pkgs.makeFontsConf { fontDirectories = with pkgs; [ noto-fonts ]; };
          tosVideo = pkgs.fetchurl {
            url = "https://ftp.nluug.nl/pub/graphics/blender/demo/movies/ToS/ToS-4k-1920.mov";
            hash = "sha256-vStbxsFtQIUDT0fvfks5OK/oa07sSsPPJoU2fTsLI7A=";
          };
          # Build: wasm-pack build --target web --out-dir dist quiche_moq_web
          #        cp quiche_moq_web/index.html quiche_moq_web/dist/
          webPlayerDist = nixnet.hostBind "/home/benedikt/Git/quiche_moq/moq_web/dist";
          config = {
            nodePackages = with pkgs; [
              (nixnet.linkFarm "host-tools" [
                {
                  name = "bin/moq";
                  path = nixnet.hostBind "/home/benedikt/Git/quiche_moq/target/release/moq";
                }
              ])
              coreutils
              ffmpeg
              python3
              chromium
            ];
            nodes = {
              server = {
                networking.interfaces.veth1.ipv4.addresses = [
                  {
                    address = "1.0.0.1";
                    prefixLength = 24;
                  }
                ];
                scripts.main = {
                  exec = ''
                    mkfifo /tmp/pv.mp4 /tmp/pa.mp4
                    ffmpeg -nostdin -y -v error -xerror -re -i ${tosVideo} \
                      -map 0:v -c:v copy -movflags empty_moov+default_base_moof -frag_duration 1 -f mp4 /tmp/pv.mp4 \
                      -map 0:a -c:a copy -movflags empty_moov+default_base_moof -frag_duration 1 -f mp4 /tmp/pa.mp4 &
                    sleep 0.1
                    # draft >= 15 negotiates via ALPN/WT-Available-Protocols, not
                    # CLIENT_SETUP's version list — which the browser's WebTransport
                    # API has no way to send (confirmed empirically: neither a
                    # `protocols` nor a `headers` constructor option changes what's
                    # actually sent over the wire). So an unpinned server can't
                    # auto-negotiate with a browser client pinned via ?draft=18 the
                    # way it can with draft < 15 clients; pin here to match.
                    RUST_LOG=debug moq --listen 443 \
                      --cert ${certs}/cert.pem --key ${certs}/key.pem \
                      --setup-version 18 \
                      --pub tos \
                      --pub-passive "tos--video#in=/tmp/pv.mp4" \
                      --pub-passive "tos--audio#in=/tmp/pa.mp4"
                  '';
                };
              };
              client = {
                shareWayland = true;
                sharePipeWire = true;
                networking.interfaces.veth1.ipv4.addresses = [
                  {
                    address = "1.0.0.2";
                    prefixLength = 24;
                  }
                ];
                scripts.main = {
                  exec = ''
                    trap 'kill $(jobs -p) 2>/dev/null' EXIT
                    python3 -m http.server 8080 --directory ${webPlayerDist} &
                    sleep 1
                    FONTCONFIG_FILE=${fontsConf} chromium \
                      --no-sandbox \
                      --origin-to-force-quic-on="*" \
                      --ignore-certificate-errors-spki-list=$(cat ${certs}/spki) \
                      --test-type \
                      "http://localhost:8080/?relay=https://1.0.0.1&draft=18"
                  '';
                  await = true;
                };
              };
            };
            veths = {
              veth1 = {
                netem.delayMs = 5;
                a.node = "client";
                b.node = "server";
              };
            };
          };
        in
        {
          packages.default = nixnet.mkExperiment config;
          packages.mermaid = nixnet.mkMermaid config;
          packages.mermaid-svg = nixnet.mkMermaidSvg config;
        };
    };
}
