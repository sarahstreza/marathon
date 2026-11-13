# NixOS module: runs marathond as a systemd service.
#
#   services.marathond = {
#     enable = true;
#     settings = {
#       server.bind = "0.0.0.0:7878";
#       server.tokenFile = "/run/secrets/marathon-token";
#       source.nas = { kind = "filesystem"; root = "/srv/video"; };
#       channel.office = {
#         number = 1;
#         name = "The Office";
#         items = [ { source = "nas"; path = "TV/The Office (US)"; } ];
#         rule.kind = "ordered";
#       };
#     };
#     openFirewall = true;
#   };
#
# `settings` has the typed options in ./settings.nix, checked when the
# system is built. The config file it becomes lands in the world-readable
# Nix store, so secrets go in files instead (`server.tokenFile`, a
# source's `apiKeyFile`); they can be readable by root only, since systemd
# hands them to the service as credentials, and they are filled in each
# time it starts. Or set `configFile` to a complete config file of your
# own.
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
  marathonLib = import ./lib.nix { inherit lib pkgs; };
  useSettings = cfg.configFile == null;
  generated = marathonLib.toConfig cfg.settings;

  # Each secret file becomes a credential named after its placeholder's
  # position, read by the start script from the credentials directory.
  secrets = lib.imap0 (i: s: s // { credential = "secret-${toString i}"; }) generated.secrets;

  start = pkgs.writeShellScript "marathond-start" ''
    set -euo pipefail
    ${
      if !useSettings then
        ''
          exec ${lib.getExe cfg.package} --config "$CREDENTIALS_DIRECTORY/config.toml"
        ''
      else if secrets == [ ] then
        ''
          exec ${lib.getExe cfg.package} --config ${generated.file}
        ''
      else
        ''
          ${marathonLib.renderSnippet {
            config = generated // {
              inherit secrets;
            };
            secretPath = s: ''"$CREDENTIALS_DIRECTORY/${s.credential}"'';
          }}
          exec ${lib.getExe cfg.package} --config /dev/fd/3
        ''
    }
  '';

  # The port to open: the last `:`-separated field of server.bind.
  port = lib.toInt (lib.last (lib.splitString ":" cfg.settings.server.bind));
in
{
  options.services.marathond = {
    enable = lib.mkEnableOption "the Marathon server (marathond)";

    package = lib.mkOption {
      type = lib.types.package;
      description = ''
        The marathond package, unwrapped: the module writes the config
        itself. The flake's module sets this to its `marathond-unwrapped`.
      '';
    };

    settings = lib.mkOption {
      type = lib.types.submodule [
        ./settings.nix
        { server.data_dir = lib.mkDefault "/var/lib/marathon"; }
      ];
      default = { };
      description = ''
        marathond's configuration; see nix/settings.nix in the Marathon
        repository for the options and config/example.toml for what they
        mean. Ignored when `configFile` is set.
      '';
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/run/secrets/marathon.toml";
      description = ''
        A complete config file to use instead of `settings`. It may be
        readable by root only. Its `server.data_dir` must be
        /var/lib/marathon (or a directory under it).
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
        assertion = cfg.openFirewall -> useSettings;
        message = ''
          services.marathond.openFirewall reads the port from `settings`; with
          `configFile`, open the port with networking.firewall.allowedTCPPorts.
        '';
      }
    ]
    ++ lib.optionals useSettings (
      map (a: a // { message = "services.marathond.settings: ${a.message}"; }) cfg.settings.assertions
    );

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ port ];

    systemd.services.marathond = {
      description = "Marathon server";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      # Restart when the config changes. (Secret files changing on their
      # own need a restart by hand.)
      restartTriggers = [ start ];

      serviceConfig = {
        ExecStart = start;
        LoadCredential =
          if useSettings then
            map (s: "${s.credential}:${s.file}") secrets
          else
            [ "config.toml:${cfg.configFile}" ];
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
