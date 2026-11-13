# The desktop player, wrapped with typed settings (./desktop-settings.nix).
# Configure it with `.override`:
#
#   marathon-desktop.override {
#     settings = {
#       server = "http://tv-server:7878";
#       tokenFile = "/run/secrets/marathon-token";
#       cec.enable = true;            # with marathon-desktop-cec
#       environment.RUST_LOG = "info";
#     };
#   }
#
# or run an embedded server from typed settings:
#
#   marathon-desktop.override {
#     settings.embedded = {
#       enable = true;
#       settings.source.nas = { kind = "filesystem"; root = "/srv/video"; };
#       settings.channel.office = { ... };
#     };
#   }
#
# With no settings the wrapper passes everything through, so this behaves
# like `marathon-desktop-unwrapped`. The desktop entry and icon come along.
{
  lib,
  pkgs,
  symlinkJoin,
  writeShellScript,
  marathon-desktop-unwrapped,
  # Typed settings: an attribute set or module (or a list of them).
  settings ? { },
}:

let
  marathonLib = import ./lib.nix { inherit lib pkgs; };
  cfg =
    marathonLib.checkAssertions "marathon-desktop settings"
      (lib.evalModules {
        modules = [ ./desktop-settings.nix ] ++ lib.toList settings;
      }).config;

  withCec = marathon-desktop-unwrapped.withCec or false;
  embedded = cfg.embedded;
  embeddedConfig =
    if !embedded.enable then
      null
    else if embedded.configFile != null then
      {
        setup = "";
        path = embedded.configFile;
      }
    else
      marathonLib.configArgs embedded.settings;

  opt =
    flag: value:
    lib.optionals (value != null) [
      flag
      (toString value)
    ];
  args =
    lib.optionals embedded.enable [
      "--embedded"
      "--config"
      embeddedConfig.path
    ]
    ++ opt "--channel" cfg.channel
    ++ lib.optional cfg.windowed "--windowed"
    ++ opt "--client-id-file" cfg.clientIdFile
    ++ lib.optionals cfg.cec.enable (
      [ "--cec" ]
      ++ opt "--cec-adapter" cfg.cec.adapter
      ++ opt "--cec-hdmi-port" cfg.cec.hdmiPort
      ++ opt "--cec-name" cfg.cec.name
      ++ lib.optional cfg.cec.claimOnStart "--cec-claim-on-start"
      ++ lib.optional cfg.cec.noResume "--cec-no-resume"
      ++ lib.concatLists (
        lib.mapAttrsToList (colour: action: [
          "--cec-button"
          "${colour}=${action}"
        ]) cfg.cec.buttons
      )
    )
    ++ cfg.extraArgs;

  # Environment defaults: the caller's own environment wins.
  setDefault = name: value: ''
    if [[ -z ''${${name}+set} ]]; then export ${name}=${lib.escapeShellArg value}; fi
  '';
  environment =
    cfg.environment
    // lib.optionalAttrs (cfg.server != null) { MARATHON_SERVER = cfg.server; }
    // lib.optionalAttrs (cfg.token != null) { MARATHON_TOKEN = cfg.token; };

  launcher = writeShellScript "marathon-desktop" ''
    set -euo pipefail
    ${lib.concatStrings (lib.mapAttrsToList setDefault environment)}
    ${lib.optionalString (cfg.tokenFile != null) ''
      if [[ -z ''${MARATHON_TOKEN+set} ]]; then
        MARATHON_TOKEN=$(< ${lib.escapeShellArg cfg.tokenFile})
        export MARATHON_TOKEN
      fi
    ''}
    ${lib.optionalString (embeddedConfig != null) embeddedConfig.setup}
    exec -a "$0" ${lib.getExe marathon-desktop-unwrapped} ${lib.escapeShellArgs args} "$@"
  '';
in
assert lib.assertMsg (
  !cfg.cec.enable || withCec
) "marathon-desktop: `cec.enable` needs the CEC build; override marathon-desktop-cec instead";
symlinkJoin {
  name = "marathon-desktop-${marathon-desktop-unwrapped.version}";
  paths = [ marathon-desktop-unwrapped ];
  postBuild = ''
    rm $out/bin/marathon-desktop
    ln -s ${launcher} $out/bin/marathon-desktop
  '';
  passthru = {
    unwrapped = marathon-desktop-unwrapped;
    # The evaluated settings, and embedded mode's generated config file
    # (with placeholders for secrets; null unless generated).
    settings = cfg;
    embeddedConfigFile =
      if embedded.enable && embedded.configFile == null then
        (marathonLib.toConfig embedded.settings).file
      else
        null;
  };
  inherit (marathon-desktop-unwrapped) version;
  meta = marathon-desktop-unwrapped.meta;
}
