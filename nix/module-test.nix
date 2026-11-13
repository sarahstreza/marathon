# NixOS VM test for nix/module.nix: the service starts with a config from
# `settings`, answers on its port, and keeps its database in its state
# directory. Run with `nix flake check` (or `nix build .#checks.<system>.module`).
{ pkgs, module }:

pkgs.testers.runNixOSTest {
  name = "marathond-module";

  nodes.machine = {
    imports = [ module ];
    systemd.tmpfiles.rules = [ "d /srv/video/TV/Show 0755 root root -" ];
    services.marathond = {
      enable = true;
      settings = {
        source = [
          {
            id = "nas";
            kind = "filesystem";
            root = "/srv/video";
          }
        ];
        channel = [
          {
            id = "show";
            number = 1;
            name = "Show";
            items = [
              {
                source = "nas";
                path = "TV/Show";
              }
            ];
            rule.kind = "ordered";
          }
        ];
      };
    };
  };

  testScript = ''
    machine.wait_for_unit("marathond.service")
    machine.wait_for_open_port(7878)
    machine.succeed("curl -sf http://127.0.0.1:7878/api/v1/health")
    channels = machine.succeed("curl -sf http://127.0.0.1:7878/api/v1/channels")
    assert '"show"' in channels, channels
    machine.succeed("test -e /var/lib/marathon/marathon.db")
  '';
}
