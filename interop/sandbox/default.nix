# The moq-interop-runner matrix of every registered implementation, run in the Nix build sandbox: the images,
# pinned by digest in pins.json, unpacked and run with bubblewrap by ./docker, which stands in for docker.
#   nix build .#interop-matrix                     -> results/ (summary.json, report.html, one log per pair)
#   nix build .#interop.pairs.<relay>--<client>    one pair, rerun only when one of its two images changes
# Pairs that fail are reported, the build succeeds. Refresh the pins with update-pins.sh.

{ pkgs, ourImage }:

let
  inherit (pkgs) lib;

  pins = lib.importJSON ./pins.json;

  # moq-interop-runner, unchanged; fetched while evaluating, its registry names the pairs
  runnerSrc = builtins.fetchTarball {
    url = "https://github.com/englishm/moq-interop-runner/archive/1c4a2283e475b360a952065a5025bd0020dd91ab.tar.gz";
    sha256 = "sha256-oMcYHDjIal59rcf+1OLrm7oW71xErwDg+94l8JHKk+o=";
  };

  # a directory name for an image reference, as ./docker looks it up
  dirName = ref: lib.replaceStrings [ "/" ":" ] [ "_" "_" ] ref;

  # an environment as the JSON list of KEY=VALUE an OCI config holds
  envJSON = env: builtins.toJSON (lib.mapAttrsToList (k: v: "${k}=${v}") env);

  pull = ref: pin:
    let
      parts = builtins.match "(.*):([^:/]+)" ref;
    in
    pkgs.dockerTools.pullImage {
      imageName = builtins.elemAt parts 0;
      finalImageTag = builtins.elemAt parts 1;
      imageDigest = pin.digest;
      hash = if pin.hash == "" then lib.fakeHash else pin.hash;
      os = "linux";
      arch = "amd64";
    };

  # rootfs/ and config.json (the process an OCI runtime spec starts) of a docker-archive, with the mount points
  # ./docker binds. The layers are applied in order with their whiteouts, without owners and setuid bits, which
  # the sandbox forbids: containers run as one user anyway.
  # - `env` is appended to the image's, `fixup` runs in the rootfs: what an adapter Dockerfile adds
  unpack = { name, archive, env ? { }, fixup ? "" }:
    pkgs.runCommand "interop-image-${dirName name}" {
      nativeBuildInputs = [ pkgs.skopeo pkgs.jq pkgs.gnutar pkgs.gzip pkgs.zstd ];
    } ''
      # no /var/tmp in the sandbox
      skopeo --insecure-policy --tmpdir "$PWD" copy --quiet docker-archive:${archive} dir:image
      mkdir rootfs
      for layer in $(jq -r '.layers[].digest | ltrimstr("sha256:")' image/manifest.json); do
        tar -tf image/$layer | while IFS= read -r entry; do
          entry=''${entry#./}
          base=$(basename "$entry")
          case "$base" in
            .wh..wh..opq) find "rootfs/$(dirname "$entry")" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true ;;
            .wh.*) rm -rf "rootfs/$(dirname "$entry")/''${base#.wh.}" ;;
          esac
        done
        # device nodes need privileges, the containers get /dev from bwrap
        tar -xf image/$layer -C rootfs --no-same-owner --no-same-permissions --delay-directory-restore --exclude='.wh.*' --exclude='*/.wh.*' 2>tar.err \
          || { grep -v 'Cannot mknod\|Exiting with failure status' tar.err >&2 && exit 1 || true; }
        chmod -R u+rwX rootfs
      done
      ( cd rootfs; ${fixup} )
      mkdir -p rootfs/{certs,mlog,etc,tmp,var/tmp,run,proc,dev}
      touch rootfs/etc/hosts
      config=image/$(jq -r '.config.digest | ltrimstr("sha256:")' image/manifest.json)
      jq --argjson env '${envJSON env}' '{ process: {
        args: ((.config.Entrypoint // []) + (.config.Cmd // [])),
        env: ((.config.Env // []) + $env),
        cwd: (if (.config.WorkingDir // "") == "" then "/" else .config.WorkingDir end)
      } }' $config > config.json
      mkdir $out
      mv rootfs config.json $out/
    '';

  # the fetches, by image reference
  archives = lib.mapAttrs pull pins;

  pulled = lib.mapAttrs (ref: archive: unpack { name = ref; inherit archive; }) archives;

  # adapters/*/Dockerfile.relay of the runner: the upstream image with environment and small fixes
  adapters = {
    "aiomoqt-interop-relay:latest" = {
      from = "ghcr.io/gmarzot/aiomoqt:interop";
      env = { MOQT_ROLE = "relay"; MOQT_PORT = "4443"; MOQT_DUAL = "1"; };
    };
    "moq-dev-rs-interop:latest" = {
      from = "docker.io/moqdev/moq-relay:latest";
      env = {
        MOQ_LISTEN = "[::]:4443";
        MOQ_LISTEN_TLS_CERT = "/certs/cert.pem";
        MOQ_LISTEN_TLS_KEY = "/certs/priv.key";
        MOQ_AUTH_PUBLIC = "**";
      };
    };
    "moqx-interop:latest" = {
      from = "ghcr.io/openmoq/moqx:latest";
      env = {
        MOQX_PORT = "4443";
        MOQX_BIND_ADDR = "0.0.0.0";
        MOQX_CERT = "/certs/cert.pem";
        MOQX_KEY = "/certs/priv.key";
        MOQX_ENDPOINT = "/";
        MOQX_QLOG_DIR = "/tmp/moqx-qlog";
      };
    };
    "moxygen-interop:latest" = {
      from = "ghcr.io/facebookexperimental/moqrelay:latest-amd64";
      env = {
        MOQ_PORT = "4443";
        CERT_FILE = "/certs/cert.pem";
        KEY_FILE = "/certs/priv.key";
        MOQ_ENDPOINT = "/";
        MOQ_LOG_LEVEL = "DBG2";
      };
      fixup = ''
        for root in tmp/fbcode_builder_getdeps-*-root; do
          if [ -d "$root" ]; then ln -sfn "/$root" "''${root%-root}"; fi
        done
      '';
    };
  };

  adapted = lib.mapAttrs (name: a: unpack {
    inherit name;
    archive = archives.${a.from};
    env = a.env;
    fixup = a.fixup or "";
  }) adapters;

  ours = unpack { name = "birneee-moq:latest"; archive = ourImage; };

  # the unpacked images by reference
  images = pulled // adapted // { "birneee-moq:latest" = ours; };

  # the relay only by IPv4, as in compose's network: relays may not listen on IPv6
  hosts = pkgs.writeText "hosts" ''
    127.0.0.1 localhost relay
    ::1 localhost
  '';

  docker = pkgs.writeShellApplication {
    name = "docker";
    runtimeInputs = [ pkgs.bubblewrap pkgs.jq pkgs.gnused pkgs.gnugrep pkgs.coreutils ];
    text = builtins.readFile ./docker;
    checkPhase = "";
  };

  # the registry with our implementation
  registry = lib.recursiveUpdate (lib.importJSON "${runnerSrc}/implementations.json") {
    implementations.birneee-moq = {
      name = "birneee-moq (quiche_moq)";
      organization = "Benedikt Spies";
      repository = "https://github.com/birneee/quiche_moq";
      draft_versions = [ "draft-18" ];
      roles = {
        relay.docker.image = "birneee-moq:latest";
        client.docker.image = "birneee-moq:latest";
      };
    };
  };

  dockerImage = role: impl: registry.implementations.${impl}.roles.${role}.docker.image or null;
  withRole = role: lib.filter (impl: dockerImage role impl != null) (lib.attrNames registry.implementations);
  # the runner only tests a pair sharing a draft
  sharesDraft = relay: client:
    lib.intersectLists registry.implementations.${relay}.draft_versions registry.implementations.${client}.draft_versions != [ ];

  # the runner with our registry, runnable in the sandbox
  runner = pkgs.runCommand "moq-interop-runner" { } ''
    cp -r ${runnerSrc} $out
    chmod -R u+w $out
    cp ${pkgs.writeText "implementations.json" (builtins.toJSON registry)} $out/implementations.json
    patchShebangs $out
    # the sandbox has no /bin/bash
    substituteInPlace $out/Makefile --replace-fail "SHELL := /bin/bash" "SHELL := ${pkgs.bash}/bin/bash"
  '';

  tools = with pkgs; [ bash coreutils findutils gnugrep gnused gawk gnumake jq openssl procps ];

  # a writable copy of the runner as the working directory of a build
  # - without a trailing newline: the builders stay as they were, a pair's derivation reruns only for its images
  enterRunner = lib.concatStringsSep "\n" [ "cp -r ${runner} runner" "chmod -R u+w runner" "cd runner" ];

  # an image with more environment, e.g. RUST_LOG for debugging: the rootfs stays, the config changes
  withEnv = ref: env:
    if env == { } then images.${ref} else
    pkgs.runCommand "interop-image-${dirName ref}-env" { nativeBuildInputs = [ pkgs.jq ]; } ''
      mkdir $out
      ln -s ${images.${ref}}/rootfs $out/rootfs
      jq --argjson env '${envJSON env}' '.process.env += $env' \
        ${images.${ref}}/config.json > $out/config.json
    '';

  # The runner on one pair, with only its two images: a pair reruns only when one of them changes.
  # - results/: the runner's shard of the pair (summary.json, one log per endpoint), and run.log
  # - `relayEnv`, `clientEnv`: more environment for debugging, merged when both use the same image
  runPair = { relay, client, relayEnv ? { }, clientEnv ? { } }:
    let
      relayRef = dockerImage "relay" relay;
      clientRef = dockerImage "client" client;
      envOf = ref: lib.optionalAttrs (ref == relayRef) relayEnv // lib.optionalAttrs (ref == clientRef) clientEnv;
      refs = lib.filter (ref: images ? ${ref}) (lib.unique [ relayRef clientRef ]);
    in
    pkgs.runCommand "moq-interop-${relay}--${client}" {
      # the images are amd64
      meta.platforms = [ "x86_64-linux" ];
      nativeBuildInputs = tools ++ [ docker ];
      INTEROP_IMAGES = pkgs.linkFarm "interop-images-${relay}--${client}" (map (ref: { name = dirName ref; path = withEnv ref (envOf ref); }) refs);
      INTEROP_HOSTS = hosts;
      TEST_TIMEOUT = "120";
    } ''
      ${enterRunner}
      export HOME=$TMPDIR
      # a failing pair makes the runner exit non-zero, it is in the results
      ./run-interop-tests.sh --docker-only --relay ${relay} --client ${client} 2>&1 | tee run.log || true
      mkdir -p $out
      cp -r results/*/ $out/results
      cp run.log $out/
    '';

  # every pair the runner tests, by `<relay>--<client>`
  pairs = lib.listToAttrs (lib.concatMap (relay: map (client: lib.nameValuePair "${relay}--${client}" (runPair { inherit relay client; }))
    (lib.filter (sharesDraft relay) (withRole "client"))) (withRole "relay"));
in
{
  # runPair for debugging, e.g. (runPair { relay = "birneee-moq"; client = "moq-rs-draft-18"; relayEnv.RUST_LOG = "debug"; })
  # archives for update-pins.sh
  inherit images runnerSrc archives pairs runPair;

  # the pairs' results merged as one runner run: results/ with summary.json, report.html and every log
  matrix = pkgs.runCommand "moq-interop-matrix" {
    meta.platforms = [ "x86_64-linux" ];
    nativeBuildInputs = tools;
  } ''
    ${enterRunner}
    ./merge-results.sh results/merged ${lib.concatMapStringsSep " " (pair: "${pair}/results") (lib.attrValues pairs)}
    ./generate-report.sh results/merged
    mkdir -p $out
    cp -r results/merged $out/results
  '';
}
