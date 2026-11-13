# Typed options for the wrapped desktop player (./marathon-desktop-wrapped.nix),
# for lib.evalModules. Each maps onto a marathon-desktop command-line flag
# or environment variable (`marathon-desktop --help`); embedded mode's
# server config uses ./settings.nix.
{ lib, config, ... }:

let
  inherit (lib) mkOption types;

  nullOr =
    type: description:
    mkOption {
      type = types.nullOr type;
      default = null;
      inherit description;
    };
in
{
  options = {
    server = nullOr types.str ''
      Base URL of the Marathon server (MARATHON_SERVER). Default:
      http://127.0.0.1:7878.
    '';
    token = nullOr types.str ''
      Bearer token for the server (MARATHON_TOKEN). It ends up in the Nix
      store; prefer `tokenFile`.
    '';
    tokenFile = nullOr types.str "A file holding the bearer token, read at launch.";
    channel = nullOr types.ints.unsigned "Channel number to start on (default: the last one watched).";
    windowed = mkOption {
      type = types.bool;
      default = false;
      description = "Start in a window rather than fullscreen.";
    };
    clientIdFile = nullOr types.str "Where the client ID is stored (default: marathon/client-id in the config directory).";

    embedded = {
      enable = mkOption {
        type = types.bool;
        default = false;
        description = "Run a server inside the player (--embedded) instead of connecting to one.";
      };
      settings = mkOption {
        type = types.submodule ./settings.nix;
        default = { };
        description = ''
          The embedded server's config (the same options as marathond's).
          `server.bind` is ignored: it listens on a loopback port the OS
          picks.
        '';
      };
      configFile = nullOr types.str "An existing config file for embedded mode, instead of `settings`.";
    };

    cec = {
      enable = mkOption {
        type = types.bool;
        default = false;
        description = "Use the TV's remote over HDMI-CEC (--cec). Needs the CEC build (marathon-desktop-cec).";
      };
      adapter = nullOr types.str "CEC adapter, e.g. /dev/cec0 (default: the first found).";
      hdmiPort = nullOr (types.ints.between 1 15) "The TV's HDMI input Marathon is on, if it cannot be detected.";
      name = nullOr types.str "The name the TV shows for the input (at most 14 characters).";
      claimOnStart = mkOption {
        type = types.bool;
        default = false;
        description = "Switch the TV to Marathon (turning it on) at startup.";
      };
      noResume = mkOption {
        type = types.bool;
        default = false;
        description = "Stay paused when the TV switches back to Marathon.";
      };
      buttons = mkOption {
        type = types.attrsOf types.str;
        default = { };
        example = {
          red = "toggle_subtitles";
          blue = ''{"action":"go_to_channel","number":1}'';
        };
        description = "What each colour button does: an action name, or an action's JSON.";
      };
    };

    environment = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = {
        RUST_LOG = "info,mpv=debug";
      };
      description = "Environment variables to set, unless already set when the player starts.";
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "More command-line arguments, before the caller's.";
    };

    assertions = mkOption {
      type = types.listOf types.unspecified;
      internal = true;
      default = [ ];
    };
  };

  config.assertions = [
    {
      assertion = config.token == null || config.tokenFile == null;
      message = "set `token` or `tokenFile`, not both";
    }
    {
      assertion =
        !config.embedded.enable
        || (config.server == null && config.token == null && config.tokenFile == null);
      message = "`server`, `token`, and `tokenFile` don't apply with `embedded.enable` (the embedded server's token comes from `embedded.settings`)";
    }
    {
      assertion = config.embedded.enable || config.embedded.configFile == null;
      message = "`embedded.configFile` needs `embedded.enable`";
    }
    {
      assertion =
        config.cec.enable
        || (
          config.cec.adapter == null
          && config.cec.hdmiPort == null
          && config.cec.name == null
          && !config.cec.claimOnStart
          && !config.cec.noResume
          && config.cec.buttons == { }
        );
      message = "the `cec` options need `cec.enable`";
    }
  ]
  # The embedded server's own checks, when its settings are used.
  ++ lib.optionals (config.embedded.enable && config.embedded.configFile == null) (
    map (a: a // { message = "embedded.settings: ${a.message}"; }) config.embedded.settings.assertions
  );
}
