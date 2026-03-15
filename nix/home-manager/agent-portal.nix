{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.agent-portal;
in
{
  options.services.agent-portal = {
    enable = lib.mkEnableOption "agent portal host service";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.portal;
      defaultText = lib.literalExpression "self.packages.${pkgs.system}.portal";
      description = "Package providing the `agent-portal-host` binary.";
    };

    cliPackage = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.cli;
      defaultText = lib.literalExpression "self.packages.${pkgs.system}.cli";
      description = "Package providing the `agent-portal-cli` binary.";
    };

    installCli = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to install the `agent-portal-cli` binary into home.packages.";
    };

    debug = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable verbose logging in agent-portal-host (RUST_LOG=debug).";
    };

    socketPath = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Optional socket path override passed as `--socket` to agent-portal-host.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = lib.optionals cfg.installCli [ cfg.cliPackage ];

    systemd.user.services.agent-portal-host = {
      Unit = {
        Description = "Agent Portal Host Service";
        After = [ "graphical-session.target" ];
        Wants = [ "graphical-session.target" ];
      };

      Service = {
        Type = "simple";
        Environment = lib.optionals cfg.debug [
          "RUST_LOG=debug"
        ];
        ExecStart =
          if cfg.socketPath == null then
            "${cfg.package}/bin/agent-portal-host"
          else
            "${cfg.package}/bin/agent-portal-host --socket ${lib.escapeShellArg cfg.socketPath}";
        Restart = "on-failure";
        RestartSec = 1;
      };

      Install = {
        WantedBy = [ "default.target" ];
      };
    };
  };
}
