# NixOS module: runs marathond as a systemd service.
#
#   services.marathond = {
#     enable = true;
#     settings = {
#       server.bind = "0.0.0.0:7878";
#       source = [ { id = "nas"; kind = "filesystem"; root = "/srv/video"; } ];
#       channel = [ ... ];
#     };
#     openFirewall = true;
#   };
#
# `settings` is the config file (config/example.toml) as Nix; it lands in
# the world-readable Nix store, so secrets (server.token, Jellyfin API
# keys) belong in a config file outside the store instead: set
# `configFile` to its path (from agenix, sops-nix, or written by hand). It
# can be readable by root only; systemd hands it to the service as a
# credential.
#
# The service runs as a transient user (DynamicUser) with its database in
# /var/lib/marathon. It can read world-readable media; add groups that own
# the media to `supplementaryGroups`. Everything else is read-only to it.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.marathond;
  toml = pkgs.formats.toml { };
  configFile =
    if cfg.configFile != null then cfg.configFile else toml.generate "marathon.toml" cfg.settings;
  # The port to open: the last `:`-separated field of server.bind.
  port = lib.toInt (lib.last (lib.splitString ":" cfg.settings.server.bind));
in
{
  options.services.marathond = {
    enable = lib.mkEnableOption "the Marathon server (marathond)";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The marathond package. The flake's module sets this to its own build.";
    };

    settings = lib.mkOption {
      type = lib.types.submodule {
        freeformType = toml.type;
        options.server = {
          bind = lib.mkOption {
            type = lib.types.str;
            default = "127.0.0.1:7878";
            description = ''
              Address and port to listen on. A non-loopback address needs
              `server.token`, which should then come from `configFile`.
            '';
          };
          data_dir = lib.mkOption {
            type = lib.types.str;
            default = "/var/lib/marathon";
            description = "Where the SQLite database lives. Must be under /var/lib/marathon.";
          };
        };
      };
      default = { };
      description = ''
        marathond's configuration, written to a TOML file in the Nix store.
        See config/example.toml in the Marathon repository for the format.
        Ignored when `configFile` is set.
      '';
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/run/secrets/marathon.toml";
      description = ''
        A complete config file to use instead of `settings`, for configs
        that hold secrets. Its `server.data_dir` must be /var/lib/marathon
        (or a directory under it).
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the port in `settings.server.bind` in the firewall.";
    };

    supplementaryGroups = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "media" ];
      description = "Extra groups for the service, e.g. one that can read the video library.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.openFirewall -> cfg.configFile == null;
        message = ''
          services.marathond.openFirewall reads the port from `settings`; with
          `configFile`, open the port with networking.firewall.allowedTCPPorts.
        '';
      }
    ];

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ port ];

    systemd.services.marathond = {
      description = "Marathon server";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      # Restart when the config changes.
      restartTriggers = [ configFile ];

      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --config \${CREDENTIALS_DIRECTORY}/config.toml";
        LoadCredential = [ "config.toml:${configFile}" ];
        Restart = "on-failure";

        DynamicUser = true;
        SupplementaryGroups = cfg.supplementaryGroups;
        StateDirectory = "marathon";
        StateDirectoryMode = "0700";
        WorkingDirectory = "/var/lib/marathon";

        # The server reads media and writes only its state directory.
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        LockPersonality = true;
        NoNewPrivileges = true;
        MemoryDenyWriteExecute = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
        ];
        UMask = "0077";
      };
    };
  };
}
