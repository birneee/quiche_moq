# The moq-interop-runner image of quiche_moq, one for both roles: MOQT_ROLE=relay runs entrypoint-relay.sh,
# else interop-client.sh (the runner's convention). Built with dockerTools from the moq-cli derivation.

{ pkgs, moq-cli }:

let
  # only the scripts the image runs, so that editing the rest of interop/ keeps the image and its interop pairs
  interopSrc = pkgs.lib.fileset.toSource {
    root = ./.;
    fileset = pkgs.lib.fileset.unions [ ./entrypoint-relay.sh ./interop-client.sh ./lib.sh ./tests ];
  };

  entrypoint = pkgs.writeShellApplication {
    name = "interop";
    runtimeInputs = [ moq-cli ];
    text = ''
      if [ "''${MOQT_ROLE:-}" = relay ]; then
        exec bash ${interopSrc}/entrypoint-relay.sh "$@"
      fi
      exec bash ${interopSrc}/interop-client.sh "$@"
    '';
  };
in
{
  image = pkgs.dockerTools.buildImage {
    name = "birneee-moq";
    tag = "latest";
    # what the scripts run in the container
    contents = [
      pkgs.cacert
      pkgs.bash
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gnused
    ];
    # for mktemp, dockerTools images have no /tmp
    extraCommands = "mkdir -p -m 1777 tmp";
    config = {
      Entrypoint = [ "${entrypoint}/bin/interop" ];
      ExposedPorts = {
        "4443/udp" = { };
      };
    };
  };
}
