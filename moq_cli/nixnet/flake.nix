{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixnet.url = "github:birneee/nixnet";
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
          fontsConf = pkgs.makeFontsConf { fontDirectories = with pkgs; [ noto-fonts ]; };
          tosVideo = pkgs.fetchurl {
            url = "https://ftp.nluug.nl/pub/graphics/blender/demo/movies/ToS/ToS-4k-1920.mov";
            hash = "sha256-vStbxsFtQIUDT0fvfks5OK/oa07sSsPPJoU2fTsLI7A=";
          };
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
            ];
            nodes = {
              client = {
                shareWayland = true;
                sharePipeWire = true;
                networking.interfaces.veth1.ipv4.addresses = [
                  {
                    address = "1.0.0.1";
                    prefixLength = 24;
                  }
                ];
                scripts.main = {
                  exec = ''
                    trap 'kill $(jobs -p) 2>/dev/null' EXIT
                    mkfifo /tmp/sv.mp4 /tmp/sa.mp4
                    sleep 1
                    moq --connect https://1.0.0.2 \
                      --sub "tos--video#out=/tmp/sv.mp4&filter=next-group-start" \
                      --sub "tos--audio#out=/tmp/sa.mp4&filter=next-group-start" &
                    ffmpeg -nostdin -v error -xerror -i /tmp/sv.mp4 -i /tmp/sa.mp4 -c copy -f matroska - | FONTCONFIG_FILE=${fontsConf} ffplay -v error -autoexit -
                  '';
                  await = true;
                };
              };
              relay = {
                networking.interfaces = {
                  veth1.ipv4.addresses = [
                    {
                      address = "1.0.0.2";
                      prefixLength = 24;
                    }
                  ];
                  veth2.ipv4.addresses = [
                    {
                      address = "2.0.0.1";
                      prefixLength = 24;
                    }
                  ];
                };
                scripts.main = {
                  exec = ''
                    moq \
                    --listen 443 \
                    --sub-passive "*" \
                    --pub-passive "*" \
                    --cache-max-obj 1048576 \
                    --cache-avg-obj 65536 \
                    --cache-depth 64
                  '';
                };
              };
              server = {
                networking.interfaces.veth2.ipv4.addresses = [
                  {
                    address = "2.0.0.2";
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
                    moq --connect https://2.0.0.1 \
                      --pub tos \
                      --pub-passive "tos--video#in=/tmp/pv.mp4" \
                      --pub-passive "tos--audio#in=/tmp/pa.mp4"
                  '';
                };
              };
            };
            veths = {
              veth1 = {
                netem.delayMs = 5;
                a.node = "client";
                b.node = "relay";
              };
              veth2 = {
                netem.delayMs = 5;
                a.node = "relay";
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
