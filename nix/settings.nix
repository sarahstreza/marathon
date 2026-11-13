# Typed options for a marathond config file (config/example.toml), as a
# module for lib.evalModules. Used by the wrapped packages
# (./marathond-wrapped.nix, ./marathon-desktop-wrapped.nix for embedded
# mode) and by the NixOS module (./module.nix); ./lib.nix turns the result
# into TOML.
#
# Key names follow the TOML (`data_dir`, `api_key`) so the example config
# and docs carry over. Two differences:
#
# - Sources and channels are attribute sets keyed by ID (`source.nas`,
#   `channel.office`) rather than lists, so another module can change one
#   (`channel.office.number = 5`).
# - Secrets can come from files read at launch (`server.tokenFile`, a
#   source's `apiKeyFile`) instead of sitting in the world-readable Nix
#   store. camelCase marks these Nix-only options.
#
# The server's own checks (crates/marathon-server/src/config.rs) still
# run when it starts; `assertions` catches the common mistakes earlier,
# at evaluation time.
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

  # Where a secret comes from: a path read at launch, not copied to the
  # store. A string, not a Nix path, so it never is.
  secretFile = description: nullOr types.str description;

  source =
    { name, config, ... }:
    {
      options = {
        id = mkOption {
          type = types.str;
          default = name;
          defaultText = lib.literalExpression "<name>";
          description = "The source's ID, which channel items refer to. Defaults to the attribute name.";
        };
        kind = mkOption {
          type = types.enum [
            "filesystem"
            "jellyfin"
          ];
          description = "A local folder (`filesystem`, including mounted shares) or a Jellyfin server.";
        };
        root = nullOr types.str "filesystem: the folder holding the videos.";
        url = nullOr types.str "jellyfin: the server's base URL, e.g. `http://jellyfin.lan:8096`.";
        api_key = nullOr types.str ''
          jellyfin: an API key (Dashboard → API Keys). It ends up in the Nix
          store; prefer `apiKeyFile`.
        '';
        apiKeyFile = secretFile "jellyfin: a file holding the API key, read at launch.";
        libraries = nullOr (types.listOf types.str) "jellyfin: only these libraries (default: all).";
      };
    };

  item = {
    options = {
      source = nullOr types.str "The source to select from (with `path`).";
      path = nullOr types.str "A folder (or file) in the source, relative to its root.";
      recursive = nullOr types.bool "Include every subfolder at any depth.";
      files = nullOr (types.listOf types.str) "Exactly these files or folders under `path`, in this order.";
      exclude = nullOr (types.listOf types.str) "Leave out these files or folders under `path`.";
      channel = nullOr types.str "A child channel's ID, instead of `source` and `path`.";
      weight = nullOr types.ints.unsigned "Weight for a `weighted` rule with `by = \"manual\"`.";
    };
  };

  order = types.enum [
    "ordered"
    "shuffled"
  ];

  rule = {
    options = {
      kind = mkOption {
        type = types.enum [
          "ordered"
          "listed"
          "shuffled"
          "grouped"
          "weighted"
        ];
        description = "The programming rule; see config/example.toml.";
      };
      by = nullOr (types.enum [
        "season"
        "folder"
        "manual"
        "item_count"
        "duration"
      ]) "grouped: `season` or `folder`. weighted: `manual` (default), `item_count`, or `duration`.";
      groups = nullOr order "grouped: the order groups play in.";
      within = nullOr order "grouped: the order items play in within a group.";
    };
  };

  channel =
    { name, ... }:
    {
      options = {
        id = mkOption {
          type = types.str;
          default = name;
          defaultText = lib.literalExpression "<name>";
          description = "The channel's ID. Defaults to the attribute name.";
        };
        number = nullOr types.ints.unsigned "The channel number. Only hidden channels may go without.";
        name = mkOption {
          type = types.str;
          description = "The channel's name, shown in the guide.";
        };
        items = mkOption {
          type = types.listOf (types.submodule item);
          description = "What the channel plays: item selectors and child channels, in order.";
        };
        rule = mkOption {
          type = types.submodule rule;
          description = "How the channel picks what plays next.";
        };
        state = mkOption {
          type = types.enum [
            "shared"
            "independent"
          ];
          default = "shared";
          description = "Whether the channel shares its child channels' playback state.";
        };
        hidden = mkOption {
          type = types.bool;
          default = false;
          description = "Leave the channel out of the guide and channel up/down.";
        };
      };
    };

  loopback = bind: lib.hasPrefix "127." bind || lib.hasPrefix "[::1]:" bind;
in
{
  options = {
    server = {
      bind = mkOption {
        type = types.str;
        default = "127.0.0.1:7878";
        description = "Address and port to listen on. A non-loopback address needs a token.";
      };
      data_dir = mkOption {
        type = types.str;
        default = "~/.local/share/marathon";
        description = "Where the SQLite database lives. A leading `~` is the home directory.";
      };
      token = nullOr types.str ''
        Bearer token clients must send. It ends up in the Nix store; prefer
        `tokenFile`.
      '';
      tokenFile = secretFile "A file holding the bearer token, read at launch.";
    };

    source = mkOption {
      type = types.attrsOf (types.submodule source);
      default = { };
      description = "Where videos come from, by ID.";
    };

    channel = mkOption {
      type = types.attrsOf (types.submodule channel);
      default = { };
      description = "The channels, by ID.";
    };

    assertions = mkOption {
      type = types.listOf types.unspecified;
      internal = true;
      default = [ ];
      description = "Checks run on the settings; ./lib.nix (or the NixOS module) reports failures.";
    };
  };

  config.assertions =
    let
      inherit (config) server;
      sourceChecks = lib.concatLists (
        lib.mapAttrsToList (
          name: s:
          let
            where = "source.${name}";
            has = field: s.${field} != null;
            fs = [
              "root"
            ];
            jf = [
              "url"
              "api_key"
              "apiKeyFile"
              "libraries"
            ];
            onlyFor =
              kind: fields:
              map (f: {
                assertion = !has f;
                message = "${where}: `${f}` only applies to kind = \"${kind}\"";
              }) fields;
          in
          if s.kind == "filesystem" then
            [
              {
                assertion = has "root";
                message = "${where}: a filesystem source needs `root`";
              }
            ]
            ++ onlyFor "jellyfin" jf
          else
            [
              {
                assertion = has "url";
                message = "${where}: a jellyfin source needs `url`";
              }
              {
                assertion = has "api_key" != has "apiKeyFile";
                message = "${where}: a jellyfin source needs exactly one of `api_key` and `apiKeyFile`";
              }
            ]
            ++ onlyFor "filesystem" fs
        ) config.source
      );
      channelChecks = lib.concatLists (
        lib.mapAttrsToList (
          name: c:
          let
            where = "channel.${name}";
            r = c.rule;
            ruleFields =
              {
                grouped = [
                  "by"
                  "groups"
                  "within"
                ];
                weighted = [ "by" ];
              }
              .${r.kind} or [ ];
          in
          [
            {
              assertion = c.hidden || c.number != null;
              message = "${where}: needs a `number` (only hidden channels may go without)";
            }
            {
              assertion = lib.all (f: lib.elem f ruleFields || r.${f} == null) [
                "by"
                "groups"
                "within"
              ];
              message = "${where}: rule kind \"${r.kind}\" takes only ${
                if ruleFields == [ ] then "no other fields" else lib.concatStringsSep ", " ruleFields
              }";
            }
            {
              assertion =
                r.kind != "grouped"
                || (
                  lib.elem r.by [
                    "season"
                    "folder"
                  ]
                  && r.groups != null
                  && r.within != null
                );
              message = "${where}: a grouped rule needs `by` (season or folder), `groups`, and `within`";
            }
            {
              assertion =
                r.kind != "weighted"
                || lib.elem r.by [
                  null
                  "manual"
                  "item_count"
                  "duration"
                ];
              message = "${where}: a weighted rule's `by` is manual, item_count, or duration";
            }
          ]
          ++ lib.imap1 (i: it: {
            assertion = (it.channel != null) != (it.source != null && it.path != null);
            message = "${where}: item ${toString i} needs either `channel`, or `source` and `path`";
          }) c.items
        ) config.channel
      );
    in
    [
      {
        assertion = server.token == null || server.tokenFile == null;
        message = "server: set `token` or `tokenFile`, not both";
      }
      {
        assertion = loopback server.bind || server.token != null || server.tokenFile != null;
        message = "server: bind = \"${server.bind}\" is not a loopback address, so it needs `token` or `tokenFile`";
      }
    ]
    ++ sourceChecks
    ++ channelChecks;
}
