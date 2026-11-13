# Checks for the wrapped packages (nix/*-wrapped.nix) and the typed
# settings, without a VM:
#
# - wrapped marathond starts from its settings, reading its token from a
#   file at launch (with characters that need escaping in TOML), and
#   enforces it;
# - the wrapped player renders its embedded config and accepts every flag
#   its options produce (`--version` parses them all, then exits);
# - settings with mistakes fail to evaluate, naming the problem.
#
# Run with `nix flake check` (or `nix build .#checks.<system>.wrappers`).
{
  pkgs,
  marathond,
  marathon-desktop-cec,
}:

let
  inherit (pkgs) lib;
  # Paths are relative: the test runs in the build directory, whose
  # absolute path depends on the Nix version.
  tokenFile = "token";

  channel = {
    number = 1;
    name = "Show";
    items = [
      {
        source = "lib";
        path = "TV/Show";
      }
    ];
    rule = {
      kind = "grouped";
      by = "season";
      groups = "ordered";
      within = "shuffled";
    };
  };
  library = {
    source.lib = {
      kind = "filesystem";
      root = "lib";
    };
    channel.show = channel;
  };

  server = marathond.override {
    settings = library // {
      server = {
        bind = "127.0.0.1:17878";
        data_dir = "data";
        inherit tokenFile;
      };
    };
  };

  player = marathon-desktop-cec.override {
    settings = {
      channel = 3;
      windowed = true;
      clientIdFile = "client-id";
      environment.RUST_LOG = "warn";
      embedded = {
        enable = true;
        settings = library // {
          server.tokenFile = tokenFile;
          server.data_dir = "embedded";
        };
      };
      cec = {
        enable = true;
        hdmiPort = 2;
        name = "Marathon";
        buttons.red = "toggle_subtitles";
      };
    };
  };

  # Each of these must fail to evaluate, with the message given.
  mistakes = {
    "needs `token` or `tokenFile`" = {
      server.bind = "0.0.0.0:7878";
    };
    "a filesystem source needs `root`" = {
      source.x.kind = "filesystem";
    };
    "only applies to kind = \"jellyfin\"" = {
      source.x = {
        kind = "filesystem";
        root = "/";
        url = "http://x";
      };
    };
    "a grouped rule needs" = {
      channel.x = channel // {
        rule.kind = "grouped";
      };
    };
    "needs a `number`" = {
      channel.x = removeAttrs channel [ "number" ];
    };
    "needs either `channel`, or `source` and `path`" = {
      channel.x = channel // {
        items = [ { source = "lib"; } ];
      };
    };
  };
  failsWith =
    message: settings:
    let
      result = builtins.tryEval (marathond.override { inherit settings; }).settings;
    in
    lib.assertMsg (!result.success) "settings should not evaluate (expected: ${message})";
in
assert lib.all lib.id (lib.mapAttrsToList failsWith mistakes);
pkgs.runCommand "marathon-wrappers-test"
  {
    nativeBuildInputs = [ pkgs.curl ];
  }
  ''
    mkdir -p lib/TV/Show
    printf '%s\n' 'to"k\en' > ${tokenFile}

    ${lib.getExe server} &
    pid=$!
    for _ in $(seq 50); do
      curl -s -o /dev/null http://127.0.0.1:17878/api/v1/health && break
      sleep 0.2
    done
    status=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:17878/api/v1/channels)
    [ "$status" = 401 ] || { echo "without the token: HTTP $status"; exit 1; }
    curl -sf -H 'Authorization: Bearer to"k\en' http://127.0.0.1:17878/api/v1/channels | grep -q '"show"'
    kill $pid

    # The generated configs hold a placeholder, not the token.
    for config in ${server.configFile} ${player.embeddedConfigFile}; do
      grep -q '@marathon-secret-server-token@' $config
      ! grep -q 'to"k' $config
    done

    ${lib.getExe player} --version | grep -q '^marathon-desktop '
    touch $out
  ''
