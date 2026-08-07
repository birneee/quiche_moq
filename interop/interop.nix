# interop.nix - moq-interop-runner test-harness image for quiche_moq.
#
# A single image serving both the relay and client roles, dispatched at
# runtime by interop.sh via MOQT_ROLE. Bundled as a Nix dockerTools image
# instead of a Dockerfile: reuses the moq-cli derivation directly rather
# than recompiling quiche/boring from scratch in a container build.
#
# Imported from flake.nix as:
#   import ./interop/interop.nix { inherit pkgs; moq-cli = self'.packages.moq-cli; }
# which returns { image }.

{ pkgs, moq-cli }:

let
  # The whole interop/ directory, copied into the store as-is (Nix content-
  # addresses local paths automatically). interop.sh execs its sibling
  # scripts (entrypoint-relay.sh / interop-client.sh) relative to its own
  # location, and interop-client.sh does the same for tests/*.sh, so the
  # directory layout must stay intact rather than being flattened into a
  # single generated script.
  interopSrc = ./.;

  # Thin wrapper: only job is putting `moq` on PATH before handing off to
  # the real interop.sh at its path-intact location in interopSrc.
  dispatcher = pkgs.writeShellApplication {
    name = "interop";
    runtimeInputs = [ moq-cli ];
    text = ''exec bash "${interopSrc}/interop.sh" "$@"'';
  };
in
{
  image = pkgs.dockerTools.buildImage {
    name = "birneee-moq";
    tag = "latest";
    # bash + coreutils are needed at runtime, not just build time:
    # interop.sh/interop-client.sh/tests/*.sh run *inside* the container and
    # use bash, mkdir, date, grep, sed, basename, ls, sleep, kill, cat... .
    # Plain `bash`, not `bashInteractive` - scripts are execed
    # non-interactively, no readline/completion needed.
    contents = [
      pkgs.cacert
      pkgs.bash
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gnused
    ];
    # dockerTools images have no /tmp by default, silently breaking
    # interop-client.sh's `mktemp -d` (it falls back to writing under /
    # instead of erroring, since the script doesn't use `set -e`).
    extraCommands = "mkdir -p -m 1777 tmp";
    config = {
      Entrypoint = [ "${dispatcher}/bin/interop" ];
      ExposedPorts = {
        "4443/udp" = { };
      };
    };
  };
}
