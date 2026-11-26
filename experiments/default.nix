# nixnet experiments: a server publishes Tears of Steel with moq-cli, played back
#   web: by the web player in Chromium, directly from the server
#   cli: by moq-cli and ffplay, through a moq-cli relay
#   nix run .#experiments.<name>     -mermaid, -mermaid-svg: its topology

{ pkgs, nixnet, certs, moq-cli, webPlayer }:

let
  fontsConf = pkgs.makeFontsConf { fontDirectories = with pkgs; [ noto-fonts ]; };
  # The mirror stopped serving the raw .mov directly; only a .zip
  # containing the single file ToS-4k-1920.mov remains.
  tosVideoDir = pkgs.fetchzip {
    url = "https://ftp.nluug.nl/pub/graphics/blender/demo/movies/ToS/ToS-4k-1920.mov.zip";
    hash = "sha256-d3wRk7jsJRlIKJXtslt/GlvL7esGXzKUkbIKA51WYp8=";
    stripRoot = false; # archive's single entry is the file itself, not a wrapper directory
  };

  # Publishes Tears of Steel in real time as tos--video and tos--audio with `moq`, which says where.
  # - `video`: ffmpeg's video codec options; the file's video is open GOP, only its first keyframe is an IDR frame
  publishTos = { moq, video ? "-c:v copy" }: ''
    mkfifo /tmp/pv.mp4 /tmp/pa.mp4
    ffmpeg -nostdin -y -v error -xerror -re -i ${tosVideoDir}/ToS-4k-1920.mov \
      -map 0:v ${video} -movflags empty_moov+default_base_moof -frag_duration 1 -f mp4 /tmp/pv.mp4 \
      -map 0:a -c:a copy -movflags empty_moov+default_base_moof -frag_duration 1 -f mp4 /tmp/pa.mp4 &
    sleep 0.1
    ${moq} \
      --pub tos \
      --pub "tos--video#passive" \
      --pub "tos--audio#passive" \
      --in "tos--video#file=/tmp/pv.mp4" \
      --in "tos--audio#file=/tmp/pa.mp4"
  '';

  address = address: [ { inherit address; prefixLength = 24; } ];

  web = {
    nodePackages = with pkgs; [ moq-cli coreutils ffmpeg python3 chromium ];
    nodes = {
      server = {
        networking.interfaces.veth1.ipv4.addresses = address "1.0.0.1";
        scripts.main.exec = publishTos {
          # A browser can't offer MoQ versions in CLIENT_SETUP from draft 15, only through the WebTransport
          # protocol, so both sides pin draft 18.
          moq = "RUST_LOG=debug moq --listen 443 --cert ${certs}/cert.pem --key ${certs}/key.pem --setup-version 18";
          # WebCodecs starts decoding only at an IDR frame, a subscriber joins at a group: one every 0.75 s
          video = "-c:v libx264 -preset veryfast -tune zerolatency -g 18";
        };
      };
      client = {
        shareWayland = true;
        sharePipeWire = true;
        networking.interfaces.veth1.ipv4.addresses = address "1.0.0.2";
        scripts.main = {
          exec = ''
            trap 'kill $(jobs -p) 2>/dev/null' EXIT
            python3 -m http.server 8080 --directory ${webPlayer} &
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
    veths.veth1 = {
      netem.delayMs = 5;
      a.node = "client";
      b.node = "server";
    };
  };

  cli = {
    nodePackages = with pkgs; [ moq-cli coreutils ffmpeg ];
    nodes = {
      client = {
        shareWayland = true;
        sharePipeWire = true;
        networking.interfaces.veth1.ipv4.addresses = address "1.0.0.1";
        scripts.main = {
          exec = ''
            trap 'kill $(jobs -p) 2>/dev/null' EXIT
            mkfifo /tmp/sv.mp4 /tmp/sa.mp4
            sleep 1
            moq --connect https://1.0.0.2#upstream \
              --sub "tos--video#filter=next-group-start" \
              --sub "tos--audio#filter=next-group-start" \
              --out "tos--video#file=/tmp/sv.mp4" \
              --out "tos--audio#file=/tmp/sa.mp4" &
            ffmpeg -nostdin -v error -xerror -i /tmp/sv.mp4 -i /tmp/sa.mp4 -c copy -f matroska - | FONTCONFIG_FILE=${fontsConf} ffplay -v error -autoexit -
          '';
          await = true;
        };
      };
      relay = {
        networking.interfaces = {
          veth1.ipv4.addresses = address "1.0.0.2";
          veth2.ipv4.addresses = address "2.0.0.1";
        };
        scripts.main.exec = ''
          moq \
            --listen 443 \
            --sub "*" \
            --pub "*" \
            --cache-max-obj 1048576 \
            --cache-avg-obj 65536 \
            --cache-depth 64
        '';
      };
      server = {
        networking.interfaces.veth2.ipv4.addresses = address "2.0.0.2";
        scripts.main.exec = publishTos { moq = "moq --connect https://2.0.0.1"; };
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
pkgs.lib.concatMapAttrs (name: config: {
  ${name} = nixnet.mkExperiment config;
  "${name}-mermaid" = nixnet.mkMermaid config;
  "${name}-mermaid-svg" = nixnet.mkMermaidSvg config;
}) { inherit web cli; }
