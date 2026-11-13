# marathond, wrapped with a config generated from typed settings
# (./settings.nix). Configure it with `.override`:
#
#   marathond.override {
#     settings = {
#       server.tokenFile = "/run/secrets/marathon-token";
#       source.nas = { kind = "filesystem"; root = "/srv/video"; };
#       channel.office = {
#         number = 1;
#         name = "The Office";
#         items = [ { source = "nas"; path = "TV/The Office (US)"; } ];
#         rule.kind = "ordered";
#       };
#     };
#   }
#
# Then `marathond` needs no arguments. Secret files are read each time it
# starts. With neither `settings` nor `configFile` the wrapper passes
# everything through, so this behaves like `marathond-unwrapped`.
{
  lib,
  pkgs,
  symlinkJoin,
  writeShellScript,
  marathond-unwrapped,
  # Typed settings: an attribute set or module (or a list of them).
  settings ? null,
  # Or an existing config file (a path or string), used as is.
  configFile ? null,
  # Arguments added before the caller's, e.g. [ "--bind" "0.0.0.0:7878" ].
  extraArgs ? [ ],
}:

assert lib.assertMsg (
  settings == null || configFile == null
) "marathond: give `settings` or `configFile`, not both";

let
  marathonLib = import ./lib.nix { inherit lib pkgs; };
  cfg = if settings == null then null else marathonLib.evalSettings settings;
  config =
    if cfg != null then
      marathonLib.configArgs cfg
    else if configFile != null then
      {
        setup = "";
        path = toString configFile;
      }
    else
      null;
  args =
    lib.optionals (config != null) [
      "--config"
      config.path
    ]
    ++ extraArgs;

  launcher = writeShellScript "marathond" ''
    set -euo pipefail
    ${lib.optionalString (config != null) config.setup}
    exec -a "$0" ${lib.getExe marathond-unwrapped} ${lib.escapeShellArgs args} "$@"
  '';
in
symlinkJoin {
  name = "marathond-${marathond-unwrapped.version}";
  paths = [ marathond-unwrapped ];
  postBuild = ''
    rm $out/bin/marathond
    ln -s ${launcher} $out/bin/marathond
  '';
  passthru = {
    unwrapped = marathond-unwrapped;
    # The evaluated settings, and the config file generated from them
    # (with placeholders for secrets); null without `settings`.
    settings = cfg;
    configFile = if cfg == null then null else (marathonLib.toConfig cfg).file;
  };
  inherit (marathond-unwrapped) version;
  meta = marathond-unwrapped.meta;
}
