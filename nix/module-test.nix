# NixOS VM test for nix/module.nix: the service starts with a config from
# `settings`, reads its token from a root-only file (a systemd
# credential), answers on its port, and keeps its database in its state
# directory. Run with `nix flake check` (or `nix build .#checks.<system>.module`).
{ pkgs, module }:

pkgs.testers.runNixOSTest {
  name = "marathond-module";

  nodes.machine = {
    imports = [ module ];
    systemd.tmpfiles.rules = [ "d /srv/video/TV/Show 0755 root root -" ];
    # Readable by root only: the service gets it as a credential.
    environment.etc."marathon-token" = {
      text = "s3cr\"et";
      mode = "0600";
    };
    services.marathond = {
      enable = true;
      settings = {
        server.bind = "0.0.0.0:7878";
        server.tokenFile = "/etc/marathon-token";
        source.nas = {
          kind = "filesystem";
          root = "/srv/video";
        };
        channel.show = {
          number = 1;
          name = "Show";
          items = [
            {
              source = "nas";
              path = "TV/Show";
            }
          ];
          rule.kind = "ordered";
        };
      };
      openFirewall = true;
    };
  };

  testScript =
    { nodes, ... }:
    let
      # The config the module generated (the same derivation it uses).
      configFile =
        (import ./lib.nix {
          inherit pkgs;
          inherit (pkgs) lib;
        }).toConfig
          nodes.machine.services.marathond.settings;
    in
    ''
      machine.wait_for_unit("marathond.service")
      machine.wait_for_open_port(7878)
      # Every route, /health included, needs the token once one is set.
      auth = "-H 'Authorization: Bearer s3cr\"et'"
      machine.succeed(f"curl -sf {auth} http://127.0.0.1:7878/api/v1/health")
      status = machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:7878/api/v1/channels")
      assert status == "401", f"without the token: HTTP {status}"
      channels = machine.succeed(f"curl -sf {auth} http://127.0.0.1:7878/api/v1/channels")
      assert '"show"' in channels, channels
      machine.succeed("test -e /var/lib/marathon/marathon.db")
      # The generated config holds a placeholder, not the token.
      machine.succeed("grep -q marathon-secret-server-token ${configFile.file}")
      machine.fail("grep -q s3cr ${configFile.file}")
    '';
}
