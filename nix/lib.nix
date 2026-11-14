# Helpers shared by the wrapped packages and the NixOS module: evaluate
# marathond settings (./settings.nix), write them as TOML, and start a
# program with that config, filling in secrets read from files at launch.
{ lib, pkgs }:

let
  toml = pkgs.formats.toml { };

  # Drop unset (null) options, and the Nix-only ones, so the TOML holds
  # only what marathond knows about (it rejects unknown keys).
  clean = lib.filterAttrs (_: v: v != null);
in
rec {
  # Evaluates settings, given as an attribute set or a module (or a list
  # of them), against ./settings.nix. Returns the evaluated config, or
  # throws with every failed assertion.
  evalSettings =
    settings:
    checkAssertions "marathond settings"
      (lib.evalModules {
        modules = [ ./settings.nix ] ++ lib.toList settings;
      }).config;

  checkAssertions =
    what: cfg:
    let
      failed = lib.filter (a: !a.assertion) cfg.assertions;
    in
    if failed == [ ] then
      cfg
    else
      throw "${what}:\n${lib.concatMapStringsSep "\n" (a: "- ${a.message}") failed}";

  # Turns evaluated settings into `{ text, secrets }`: `text` is the config
  # file as TOML, with a placeholder string wherever a secret file was
  # given, and `secrets` lists `{ placeholder, file }` for them. With no
  # secrets the file can be used as is.
  toConfig =
    cfg:
    let
      placeholder = name: "@marathon-secret-${name}@";
      server = cfg.server;
      tokenSecret = lib.optional (server.tokenFile != null) {
        placeholder = placeholder "server-token";
        file = server.tokenFile;
      };
      sourceSecrets = lib.concatLists (
        lib.mapAttrsToList (
          name: s:
          lib.optional (s.apiKeyFile != null) {
            placeholder = placeholder "source-${name}-api-key";
            file = s.apiKeyFile;
          }
        ) cfg.source
      );

      # Only written when something is set, so marathond's defaults apply
      # (and an empty [audio] table doesn't count as configuring it).
      audio = clean {
        inherit (cfg.audio)
          enabled
          ffmpeg
          ffprobe
          bitrate_kbps
          max_streams
          ;
      };

      attrs = lib.optionalAttrs (audio != { }) { inherit audio; } // {
        server = clean {
          inherit (server) bind data_dir;
          token = if server.tokenFile != null then placeholder "server-token" else server.token;
        };
        source = lib.mapAttrsToList (
          name: s:
          clean {
            inherit (s)
              id
              kind
              root
              url
              libraries
              ;
            api_key = if s.apiKeyFile != null then placeholder "source-${name}-api-key" else s.api_key;
          }
        ) cfg.source;
        # Ordered by number (hidden, unnumbered channels last) rather than
        # by ID, so the file reads like the guide.
        channel = map (
          c:
          clean {
            inherit (c)
              id
              number
              name
              state
              hidden
              ;
            items = map clean c.items;
            rule = clean c.rule;
          }
        ) (lib.sortOn (c: if c.number == null then 4294967296 else c.number) (lib.attrValues cfg.channel));
      };
    in
    {
      file = toml.generate "marathon.toml" attrs;
      secrets = tokenSecret ++ sourceSecrets;
    };

  # A shell snippet that makes file descriptor 3 hold the config, for
  # `--config /dev/fd/3`: the template with each secret's placeholder
  # replaced by the file's contents. Nothing is written to disk, and the
  # secrets never reach the Nix store. `secretPath` maps a secret to the
  # shell word naming its file (the NixOS module points it at systemd's
  # credentials directory).
  renderSnippet =
    {
      config,
      secretPath ? s: lib.escapeShellArg s.file,
    }:
    ''
      marathon_config=$(< ${config.file})
      marathon_secret() {
        local value
        value=$(< "$2") || { echo "marathon: cannot read secret file $2" >&2; exit 1; }
        if [[ -z $value || $value == *$'\n'* ]]; then
          echo "marathon: secret file $2 must hold one non-empty line" >&2
          exit 1
        fi
        # The placeholder sits inside a TOML basic string; escape for it.
        value=''${value//\\/\\\\}
        value=''${value//\"/\\\"}
        marathon_config=''${marathon_config//"$1"/"$value"}
      }
      ${lib.concatMapStrings (s: ''
        marathon_secret ${lib.escapeShellArg s.placeholder} ${secretPath s}
      '') config.secrets}
      exec 3<<<"$marathon_config"
      unset marathon_config
    '';

  # The `--config` argument for evaluated settings, plus the snippet that
  # must run first (empty when there are no secrets, and the file is
  # passed directly).
  configArgs =
    cfg:
    let
      config = toConfig cfg;
    in
    if config.secrets == [ ] then
      {
        setup = "";
        path = "${config.file}";
      }
    else
      {
        setup = renderSnippet { inherit config; };
        path = "/dev/fd/3";
      };
}
